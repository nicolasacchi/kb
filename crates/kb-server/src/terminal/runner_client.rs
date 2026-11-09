//! The unix-socket runner client: one JSON request line in, one JSON
//! reply line out, per connection.
//!
//! kb-server holds NO terminal state — every relay is built **per
//! request** from the live config (a path plus a timeout, invariant
//! #13: a config edit restarts the daemon in-process and the next
//! request picks the new socket up). The runner (`kb-termd`) owns
//! tmux, the tap, and the responder.

use std::path::PathBuf;
use std::time::Duration;

use tokio::io::{AsyncReadExt, AsyncWriteExt};

use super::wire::{RunnerReply, RunnerRequest};

/// Hard ceiling on one runner reply (a `history`/`read` payload is
/// terminal bytes; 4 MiB is far beyond any live session's scrollback
/// the relay returns).
pub(crate) const MAX_REPLY_BYTES: usize = 4 * 1024 * 1024;

/// Why a runner call failed. `Unavailable` never carries the socket
/// path (the route handler maps it to a fixed 503 body).
#[derive(Debug)]
pub(crate) enum RunnerError {
    /// Connect or I/O failed: the runner is down or the socket is
    /// missing.
    Unavailable,
    /// The deadline hit before a reply line arrived.
    Timeout,
    /// The reply exceeded [`MAX_REPLY_BYTES`].
    TooLarge,
    /// The reply was not a parseable `RunnerReply` JSON object.
    BadReply,
}

/// A per-request runner client. Cheap: a path plus a timeout, so the
/// handler builds one from the current config for every call.
pub(crate) struct UnixRunner {
    path: PathBuf,
    timeout: Duration,
}

impl UnixRunner {
    pub(crate) fn new(path: PathBuf, timeout: Duration) -> Self {
        Self { path, timeout }
    }

    /// One request per connection: connect, write one JSON line
    /// (`{"op":"<name>",...}` + `\n`), read one JSON line, close. The
    /// whole round trip is wrapped in the configured deadline.
    pub(crate) async fn call(&self, req: &RunnerRequest) -> Result<RunnerReply, RunnerError> {
        let line = serde_json::to_string(req).map_err(|_| RunnerError::BadReply)?;
        let outcome = tokio::time::timeout(self.timeout, self.round_trip(line)).await;
        match outcome {
            Err(_) => Err(RunnerError::Timeout),
            Ok(Ok(bytes)) => {
                let text = String::from_utf8(bytes).map_err(|_| RunnerError::BadReply)?;
                serde_json::from_str(&text).map_err(|_| RunnerError::BadReply)
            }
            Ok(Err(e)) => Err(e),
        }
    }

    async fn round_trip(&self, line: String) -> Result<Vec<u8>, RunnerError> {
        let mut stream = tokio::net::UnixStream::connect(&self.path)
            .await
            .map_err(|_| RunnerError::Unavailable)?;
        stream
            .write_all(line.as_bytes())
            .await
            .map_err(|_| RunnerError::Unavailable)?;
        stream
            .write_all(b"\n")
            .await
            .map_err(|_| RunnerError::Unavailable)?;

        // Bounded read: the cap is enforced DURING I/O, not after a
        // whole line is buffered. The take adapter caps the bytes the
        // socket yields at `MAX_REPLY_BYTES + 1`, so an oversize
        // reply errors without ever allocating past the cap (the
        // `+1` lets exactly-one-over and way-over both report
        // `TooLarge`). Read chunked until the newline, EOF, or the
        // cap: a bare `read_line` would buffer an unbounded line
        // before the length check.
        let reader = tokio::io::BufReader::new(stream);
        let mut limited = reader.take((MAX_REPLY_BYTES + 1) as u64);
        let mut line = Vec::new();
        let mut chunk = [0u8; 8192];
        let mut saw_newline = false;
        loop {
            let n = limited
                .read(&mut chunk)
                .await
                .map_err(|_| RunnerError::Unavailable)?;
            if n == 0 {
                break;
            }
            let read = &chunk[..n];
            if let Some(pos) = read.iter().position(|&b| b == b'\n') {
                line.extend_from_slice(&read[..=pos]);
                saw_newline = true;
                break;
            }
            line.extend_from_slice(read);
            if line.len() > MAX_REPLY_BYTES {
                return Err(RunnerError::TooLarge);
            }
        }
        if line.is_empty() && !saw_newline {
            // EOF without a reply: the runner dropped the connection.
            return Err(RunnerError::BadReply);
        }
        if line.len() > MAX_REPLY_BYTES {
            return Err(RunnerError::TooLarge);
        }
        Ok(line)
    }
}

/// What the upstream relay hands to the WebSocket layer, one decoded
/// tap.sock envelope. `Binary` bodies are the raw frame bytes (the
/// 8-byte BE offset header + payload, exactly as the tap emitted
/// them); `Text` bodies are the frame's UTF-8 text. Both are relayed
/// byte-for-byte — kb-server never re-chunks, re-orders or inspects
/// the payload.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum StreamItem {
    Binary(Vec<u8>),
    Text(String),
}

/// Why opening the upstream stream failed. `NotFound` is the runner's
/// refusal ("no such session"); `Unavailable` is any connect/IO
/// failure (the socket path never leaks into either arm's message).
#[derive(Debug)]
pub(crate) enum StreamError {
    NotFound(String),
    Unavailable,
    BadReply,
}

impl StreamError {
    pub(crate) fn message(&self) -> &'static str {
        match self {
            StreamError::NotFound(_) => "no such terminal",
            StreamError::Unavailable => "runner unavailable",
            StreamError::BadReply => "bad runner reply",
        }
    }
}

/// The upstream relay stream: the `stream` RPC connection after its
/// ok reply. [`TermStream::recv`] decodes tap.sock envelopes off the
/// wire; [`TermStream::send`] frames raw items back (each in its own
/// envelope).
pub(crate) struct TermStream {
    read: tokio::net::unix::OwnedReadHalf,
    write: tokio::net::unix::OwnedWriteHalf,
    /// Decoded envelopes not yet consumed (one `push` can yield many).
    pending: std::collections::VecDeque<StreamItem>,
    /// Raw envelope bytes not yet forming a complete envelope: the
    /// trailing partial of the last read, seeded from the reply-line
    /// leftovers. It survives across `recv` calls, so an envelope
    /// split across reads (or coalesced with a neighbour) always
    /// reassembles — a per-call buffer would drop the second
    /// envelope's head and corrupt the byte stream.
    read_buf: std::collections::VecDeque<u8>,
    /// The viewer id the runner opened this stream for (the `stream`
    /// request's `id`; the runner splices with the same id). C->S
    /// bodies carry the tap's S2C id prefix so `parseS2C` unwraps
    /// exactly what the tap itself would emit.
    viewer: String,
}

/// tap.sock envelope constants — the ONE framing on this connection
/// (`[u32 BE length][u8 kind][body]`, length counts kind+body). kb-term
/// pins the same numbers in `crates/kb-term/src/wire.rs`; the mirror
/// below is local so kb-server grows no kb-term edge.
pub(crate) const ENV_TEXT: u8 = 0;
pub(crate) const ENV_BINARY: u8 = 1;
pub(crate) const ENV_CONTROL: u8 = 2;
pub(crate) const MAX_ENVELOPE_LEN: usize = 1_048_576 + 512;

/// Decode one envelope from the front of `buf`, appending every
/// complete envelope found to `out`. Returns `Err` on a malformed
/// envelope (zero/oversize length, bad kind); a trailing partial
/// envelope stays buffered.
fn decode_envelopes(
    buf: &mut std::collections::VecDeque<u8>,
    out: &mut Vec<(u8, Vec<u8>)>,
) -> Result<(), String> {
    loop {
        if buf.len() < 4 {
            return Ok(());
        }
        let mut hdr = [0u8; 4];
        for (i, &b) in buf.iter().take(4).enumerate() {
            hdr[i] = b;
        }
        let len = u32::from_be_bytes(hdr) as usize;
        if len == 0 || len > MAX_ENVELOPE_LEN {
            return Err("bad envelope length".to_string());
        }
        if buf.len() < 4 + len {
            return Ok(());
        }
        let kind = *buf.get(4).unwrap_or(&0);
        if kind != ENV_TEXT && kind != ENV_BINARY && kind != ENV_CONTROL {
            return Err("bad envelope kind".to_string());
        }
        let body: Vec<u8> = (0..len - 1)
            .filter_map(|i| buf.get(5 + i).copied())
            .collect();
        buf.drain(..4 + len);
        out.push((kind, body));
    }
}

/// Frame one item for the splice as the tap's S2C shape
/// (`[idlen][id][frame]`, kind per variant): the tap's `parseS2C`
/// unwraps it exactly as it would a frame the tap itself emitted,
/// and the frame bytes ride verbatim inside the id prefix. A bare
/// JSON body would be misread as a length-prefixed id and close
/// the tap connection, so the prefix is not optional.
fn encode_item(item: &StreamItem, viewer: &str) -> Result<Vec<u8>, String> {
    let viewer_bytes = viewer.as_bytes();
    if viewer_bytes.is_empty() || viewer_bytes.len() > 255 {
        return Err("viewer id must be 1..=255 bytes".to_string());
    }
    let (kind, frame): (u8, &[u8]) = match item {
        StreamItem::Binary(bytes) => (ENV_BINARY, bytes),
        StreamItem::Text(text) => (ENV_TEXT, text.as_bytes()),
    };
    if 1 + 1 + viewer_bytes.len() + frame.len() > MAX_ENVELOPE_LEN {
        return Err("item exceeds the envelope cap".to_string());
    }
    let len = (1 + 1 + viewer_bytes.len() + frame.len()) as u32;
    let mut out = Vec::with_capacity(4 + 1 + 1 + viewer_bytes.len() + frame.len());
    out.extend_from_slice(&len.to_be_bytes());
    out.push(kind);
    out.push(viewer_bytes.len() as u8);
    out.extend_from_slice(viewer_bytes);
    out.extend_from_slice(frame);
    Ok(out)
}

fn strip_s2c_id(body: &[u8]) -> Option<&[u8]> {
    if body.is_empty() {
        return None;
    }
    let id_len = usize::from(body[0]);
    if body.len() < 1 + id_len {
        return None;
    }
    Some(&body[1 + id_len..])
}

impl TermStream {
    /// Read the next decoded item, or `None` at a clean EOF (or on a
    /// malformed envelope / non-UTF-8 text frame: the relay closes).
    /// Unconsumed raw bytes persist in `self.read_buf` across calls,
    /// so an envelope split across reads reassembles and a second
    /// envelope decoded in the same read is never discarded.
    pub(crate) async fn recv(&mut self) -> Option<StreamItem> {
        if let Some(item) = self.pending.pop_front() {
            return Some(item);
        }
        let mut chunk = [0u8; 16 * 1024];
        loop {
            let n = match self.read.read(&mut chunk).await {
                Ok(0) | Err(_) => return None,
                Ok(n) => n,
            };
            self.read_buf.extend(&chunk[..n]);
            let mut decoded = Vec::new();
            if decode_envelopes(&mut self.read_buf, &mut decoded).is_err() {
                return None;
            }
            for (kind, body) in decoded {
                // The runner forwards S->C bodies with their viewer-id
                // prefix; the WebSocket frame is the inner frame alone.
                // Control envelopes are not tap frames — skip them.
                let frame = match strip_s2c_id(&body) {
                    Some(f) => f.to_vec(),
                    None => body,
                };
                let item = match kind {
                    ENV_BINARY => StreamItem::Binary(frame),
                    // A text envelope that is not UTF-8 cannot be
                    // forwarded as a WebSocket text frame: it would
                    // be silently lost, breaking the verbatim relay
                    // claim. Treat it as a stream error instead.
                    ENV_TEXT => match String::from_utf8(frame) {
                        Ok(text) => StreamItem::Text(text),
                        Err(_) => return None,
                    },
                    _ => continue,
                };
                self.pending.push_back(item);
            }
            if let Some(item) = self.pending.pop_front() {
                return Some(item);
            }
        }
    }

    /// Write one item back towards the tap as a single S2C-wrapped
    /// envelope (see [`encode_item`]).
    pub(crate) async fn send(&mut self, item: StreamItem) -> Result<(), StreamError> {
        let bytes = encode_item(&item, &self.viewer).map_err(|_| StreamError::BadReply)?;
        self.write
            .write_all(&bytes)
            .await
            .map_err(|_| StreamError::Unavailable)
    }
}

impl UnixRunner {
    /// Open the relay stream for `id` (optional resume offset): sends
    /// the `stream` request line, reads the ok reply, and returns the
    /// live spliced connection. The runner answers the request line
    /// first, so a runner refusal surfaces here as `NotFound` (or
    /// `BadReply`) rather than mid-relay.
    pub(crate) async fn open_stream(
        &self,
        id: &str,
        from: Option<u64>,
    ) -> Result<TermStream, StreamError> {
        let req = RunnerRequest::Stream {
            id: id.to_string(),
            from,
        };
        let line = serde_json::to_string(&req).map_err(|_| StreamError::BadReply)?;
        let mut stream = tokio::net::UnixStream::connect(&self.path)
            .await
            .map_err(|_| StreamError::Unavailable)?;
        stream
            .write_all(line.as_bytes())
            .await
            .map_err(|_| StreamError::Unavailable)?;
        stream
            .write_all(b"\n")
            .await
            .map_err(|_| StreamError::Unavailable)?;
        // Read exactly one reply line (bounded like `round_trip`).
        let (reply, rest) = read_reply_line(&mut stream).await?;
        let text = String::from_utf8(reply).map_err(|_| StreamError::BadReply)?;
        match serde_json::from_str::<RunnerReply>(text.trim()).map_err(|_| StreamError::BadReply)? {
            RunnerReply::Ok { .. } => {}
            RunnerReply::Err { code, message }
                if code == "not-found" || code == "no-session" || code == "invalid-id" =>
            {
                return Err(StreamError::NotFound(message));
            }
            RunnerReply::Err { .. } => return Err(StreamError::BadReply),
        }
        // Any bytes the reply read buffered past the newline are the
        // first spliced envelopes; seed the stream with them.
        let (pending, read_buf) = decode_seed(rest);
        let (read, write) = stream.into_split();
        Ok(TermStream {
            read,
            write,
            pending,
            read_buf,
            viewer: id.to_string(),
        })
    }
}

/// Read one bounded reply line off the stream; returns the line and
/// any bytes already buffered past it.
async fn read_reply_line(
    stream: &mut tokio::net::UnixStream,
) -> Result<(Vec<u8>, Vec<u8>), StreamError> {
    let mut line = Vec::new();
    let mut rest = Vec::new();
    let mut chunk = [0u8; 8192];
    loop {
        let n = tokio::time::timeout(Duration::from_secs(30), stream.read(&mut chunk))
            .await
            .map_err(|_| StreamError::Unavailable)?
            .map_err(|_| StreamError::Unavailable)?;
        if n == 0 {
            return Err(StreamError::BadReply);
        }
        let read = &chunk[..n];
        match read.iter().position(|&b| b == b'\n') {
            Some(pos) => {
                line.extend_from_slice(&read[..=pos]);
                rest.extend_from_slice(&read[pos + 1..]);
                if line.len() > MAX_REPLY_BYTES {
                    return Err(StreamError::BadReply);
                }
                return Ok((line, rest));
            }
            None => {
                line.extend_from_slice(read);
                if line.len() > MAX_REPLY_BYTES {
                    return Err(StreamError::BadReply);
                }
            }
        }
    }
}

/// Decode any envelopes already buffered past the reply line, as
/// `(pending, leftover)`: complete envelopes seed the pending
/// queue; a trailing partial envelope is returned as raw bytes,
/// which seeds the stream's persistent read buffer (the head of
/// the next envelope, never dropped).
fn decode_seed(
    rest: Vec<u8>,
) -> (
    std::collections::VecDeque<StreamItem>,
    std::collections::VecDeque<u8>,
) {
    let mut out = std::collections::VecDeque::new();
    let mut buf: std::collections::VecDeque<u8> = rest.iter().copied().collect();
    if buf.is_empty() {
        return (out, buf);
    }
    let mut decoded = Vec::new();
    if decode_envelopes(&mut buf, &mut decoded).is_ok() {
        for (kind, body) in decoded {
            let frame = strip_s2c_id(&body).map(<[u8]>::to_vec).unwrap_or(body);
            let item = match kind {
                ENV_BINARY => StreamItem::Binary(frame),
                ENV_TEXT => match String::from_utf8(frame) {
                    Ok(text) => StreamItem::Text(text),
                    Err(_) => continue,
                },
                _ => continue,
            };
            out.push_back(item);
        }
    }
    (out, buf)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    async fn serve_one(socket: std::path::PathBuf, reply: &'static str) {
        let listener = tokio::net::UnixListener::bind(&socket).unwrap();
        tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut buf = vec![0u8; 4096];
            let _ = stream.read(&mut buf).await;
            let _ = stream.write_all(reply.as_bytes()).await;
            let _ = stream.write_all(b"\n").await;
        });
    }

    #[tokio::test]
    async fn unix_runner_roundtrips_over_a_real_socket() {
        let dir = tempfile::tempdir().unwrap();
        let sock = dir.path().join("runner.sock");
        serve_one(sock.clone(), "{\"status\":\"ok\",\"data\":{}}").await;
        let runner = UnixRunner::new(sock, Duration::from_secs(5));
        let reply = runner.call(&RunnerRequest::List {}).await.unwrap();
        match reply {
            RunnerReply::Ok { data } => assert!(data.is_object()),
            RunnerReply::Err { .. } => panic!("must not err"),
        }
    }

    #[tokio::test]
    async fn silent_server_times_out_within_one_second() {
        // The only runner-client test allowed to wait: the deadline
        // must fire.
        let dir = tempfile::tempdir().unwrap();
        let sock = dir.path().join("runner.sock");
        let listener = tokio::net::UnixListener::bind(&sock).unwrap();
        tokio::spawn(async move {
            let (_stream, _) = listener.accept().await.unwrap();
            // Never answer; hold the connection open.
            tokio::time::sleep(Duration::from_secs(60)).await;
        });
        let runner = UnixRunner::new(sock, Duration::from_secs(1));
        let started = std::time::Instant::now();
        let err = runner.call(&RunnerRequest::List {}).await;
        assert!(matches!(err, Err(RunnerError::Timeout)), "got {err:?}");
        assert!(started.elapsed() < Duration::from_secs(5));
    }

    #[tokio::test]
    async fn oversize_reply_is_too_large() {
        let dir = tempfile::tempdir().unwrap();
        let sock = dir.path().join("runner.sock");
        let big = "x".repeat(MAX_REPLY_BYTES + 1);
        let reply = format!("{{\"status\":\"ok\",\"data\":\"{big}\"}}");
        // Leaked so the spawned server task gets a 'static reply.
        serve_one(sock.clone(), Box::leak(reply.into_boxed_str())).await;
        let runner = UnixRunner::new(sock, Duration::from_secs(5));
        assert!(matches!(
            runner.call(&RunnerRequest::List {}).await,
            Err(RunnerError::TooLarge)
        ));
    }

    #[tokio::test]
    async fn absent_socket_is_unavailable_without_leaking_the_path() {
        let dir = tempfile::tempdir().unwrap();
        let missing = dir.path().join("absent.sock");
        let runner = UnixRunner::new(missing, Duration::from_secs(5));
        let err = runner.call(&RunnerRequest::List {}).await;
        match err {
            Err(RunnerError::Unavailable) => {}
            other => panic!("expected Unavailable, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn malformed_reply_is_bad_reply() {
        let dir = tempfile::tempdir().unwrap();
        let sock = dir.path().join("runner.sock");
        serve_one(sock.clone(), "not json").await;
        let runner = UnixRunner::new(sock, Duration::from_secs(5));
        assert!(matches!(
            runner.call(&RunnerRequest::List {}).await,
            Err(RunnerError::BadReply)
        ));
    }

    /// Encode one envelope exactly as the tap would (for the fake
    /// runner below): `[u32 BE length][kind][idlen, id, frame]`.
    fn fake_envelope(kind: u8, viewer: &str, frame: &[u8]) -> Vec<u8> {
        let mut body = Vec::new();
        body.push(viewer.len() as u8);
        body.extend_from_slice(viewer.as_bytes());
        body.extend_from_slice(frame);
        let mut out = Vec::new();
        out.extend_from_slice(&((1 + body.len()) as u32).to_be_bytes());
        out.push(kind);
        out.extend_from_slice(&body);
        out
    }

    #[tokio::test]
    async fn stream_open_and_recv_decode_tap_envelopes() {
        // A fake runner that answers the `stream` line with ok, then
        // splices two tap envelopes (text / binary). The framing is the
        // tap's own: S->C bodies carry a viewer-id prefix (handed off
        // to `strip_s2c_id`) and the binary frame carries its 8-byte
        // BE offset header inside the frame bytes.
        let dir = tempfile::tempdir().unwrap();
        let sock = dir.path().join("runner.sock");
        let listener = tokio::net::UnixListener::bind(&sock).unwrap();
        tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut buf = vec![0u8; 4096];
            let _ = stream.read(&mut buf).await;
            let _ = stream.write_all(b"{\"status\":\"ok\",\"data\":{}}\n").await;
            let text = fake_envelope(0, "v", br#"{"t":"ping"}"#);
            let _ = stream.write_all(&text).await;
            let binary = fake_envelope(1, "v", &[0, 0, 0, 0, 0, 0, 0, 3, b'a', b'b', b'c']);
            let _ = stream.write_all(&binary).await;
        });
        let runner = UnixRunner::new(sock, Duration::from_secs(5));
        let mut stream = runner
            .open_stream("abcd1234", None)
            .await
            .expect("open stream");
        let first = tokio::time::timeout(Duration::from_secs(5), stream.recv())
            .await
            .expect("recv 1 deadline")
            .expect("first item");
        assert_eq!(first, StreamItem::Text(r#"{"t":"ping"}"#.to_string()));
        let second = tokio::time::timeout(Duration::from_secs(5), stream.recv())
            .await
            .expect("recv 2 deadline")
            .expect("second item");
        assert_eq!(
            second,
            StreamItem::Binary(vec![0, 0, 0, 0, 0, 0, 0, 3, b'a', b'b', b'c'])
        );
    }

    #[tokio::test]
    async fn stream_open_not_found_surfaces_as_not_found() {
        let dir = tempfile::tempdir().unwrap();
        let sock = dir.path().join("runner.sock");
        let listener = tokio::net::UnixListener::bind(&sock).unwrap();
        tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut buf = vec![0u8; 4096];
            let _ = stream.read(&mut buf).await;
            let _ = stream
                .write_all(
                    b"{\"status\":\"err\",\"code\":\"not-found\",\"message\":\"no session\"}\n",
                )
                .await;
        });
        let runner = UnixRunner::new(sock, Duration::from_secs(5));
        match runner.open_stream("abcd1234", None).await {
            Err(StreamError::NotFound(_)) => {}
            other => panic!("expected NotFound, got {other:?}"),
        }
    }

    #[test]
    fn stream_framing_mirrors_t7_golden_envelope_bytes() {
        // Byte-level pin against the upstream goldens: the tap's
        // `env_ssc_text.hex` / `env_ssc_binary.hex` decode through this
        // module's decoder, with the viewer id singled out by
        // `strip_s2c_id`.
        let decode = |hex: &str| {
            let hex = hex.trim();
            assert_eq!(hex.len() % 2, 0, "hex golden must be even-length");
            let mut bytes = Vec::new();
            for i in (0..hex.len()).step_by(2) {
                bytes.push(u8::from_str_radix(&hex[i..i + 2], 16).unwrap());
            }
            let mut buf: std::collections::VecDeque<u8> = bytes.iter().copied().collect();
            let mut out = Vec::new();
            decode_envelopes(&mut buf, &mut out).unwrap();
            out
        };
        let text = decode(include_str!(
            "../../../kb-term/tests/golden/env_ssc_text.hex"
        ));
        assert_eq!(text.len(), 1);
        assert_eq!(text[0].0, ENV_TEXT);
        let frame = strip_s2c_id(&text[0].1).unwrap();
        assert_eq!(frame, br#"{"t":"ping"}"#);
        let binary = decode(include_str!(
            "../../../kb-term/tests/golden/env_ssc_binary.hex"
        ));
        assert_eq!(binary.len(), 1);
        assert_eq!(binary[0].0, ENV_BINARY);
        // The 8-byte BE offset header rides inside the frame bytes.
        let frame = strip_s2c_id(&binary[0].1).unwrap();
        assert_eq!(frame.len(), 8 + 3);
        assert_eq!(u64::from_be_bytes(frame[..8].try_into().unwrap()), 3);
    }

    #[test]
    fn stream_send_frames_each_item_as_one_envelope() {
        // C->S bodies carry the tap's S2C viewer-id prefix (handed off
        // to `parseS2C` on the tap side), with the frame bytes verbatim
        // inside it. A bare body here would close the tap connection.
        let binary = StreamItem::Binary(vec![1, 2, 3]);
        let bytes = encode_item(&binary, "v").unwrap();
        let mut buf: std::collections::VecDeque<u8> = bytes.iter().copied().collect();
        let mut out = Vec::new();
        decode_envelopes(&mut buf, &mut out).unwrap();
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].0, ENV_BINARY);
        let body = &out[0].1;
        assert_eq!(body[0], 1, "idlen carries the viewer id");
        assert_eq!(&body[1..2], b"v", "the viewer id rides with the frame");
        assert_eq!(strip_s2c_id(body).unwrap(), &[1, 2, 3]);
        let text = StreamItem::Text("hello".to_string());
        let bytes = encode_item(&text, "abcd1234").unwrap();
        let mut buf: std::collections::VecDeque<u8> = bytes.iter().copied().collect();
        let mut out = Vec::new();
        decode_envelopes(&mut buf, &mut out).unwrap();
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].0, ENV_TEXT);
        let body = &out[0].1;
        assert_eq!(body[0], 8);
        assert_eq!(&body[1..9], b"abcd1234");
        assert_eq!(strip_s2c_id(body).unwrap(), b"hello");
    }

    #[tokio::test]
    async fn stream_recv_reassembles_envelopes_split_across_reads() {
        // Regression: a per-call decode buffer dropped the trailing
        // partial of one read, so an envelope split across reads
        // (or two envelopes coalesced in one read) corrupted the
        // stream. The persistent buffer must deliver both items in
        // order, byte-for-byte, however the bytes were chunked.
        let dir = tempfile::tempdir().unwrap();
        let sock = dir.path().join("runner.sock");
        let listener = tokio::net::UnixListener::bind(&sock).unwrap();
        tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut buf = vec![0u8; 4096];
            let _ = stream.read(&mut buf).await;
            let _ = stream.write_all(b"{\"status\":\"ok\",\"data\":{}}\n").await;
            let text = fake_envelope(0, "v", br#"{"t":"ping"}"#);
            let binary = fake_envelope(1, "v", &[0, 0, 0, 0, 0, 0, 0, 3, b'a', b'b', b'c']);
            let mut all = text;
            all.extend_from_slice(&binary);
            // One byte at a time: every read boundary is a split
            // inside an envelope.
            for byte in all {
                if stream.write_all(std::slice::from_ref(&byte)).await.is_err() {
                    return;
                }
                tokio::task::yield_now().await;
            }
        });
        let runner = UnixRunner::new(sock, Duration::from_secs(5));
        let mut stream = runner
            .open_stream("abcd1234", None)
            .await
            .expect("open stream");
        let first = tokio::time::timeout(Duration::from_secs(5), stream.recv())
            .await
            .expect("recv 1 deadline")
            .expect("first item");
        assert_eq!(first, StreamItem::Text(r#"{"t":"ping"}"#.to_string()));
        let second = tokio::time::timeout(Duration::from_secs(5), stream.recv())
            .await
            .expect("recv 2 deadline")
            .expect("second item");
        assert_eq!(
            second,
            StreamItem::Binary(vec![0, 0, 0, 0, 0, 0, 0, 3, b'a', b'b', b'c'])
        );
    }

    #[test]
    fn decode_seed_keeps_trailing_partial() {
        // Bytes past the reply newline that form only a partial
        // envelope must survive as the read-buffer seed — not be
        // dropped with the per-call buffer that used to lose them.
        let full = fake_envelope(0, "v", br#"{"t":"ping"}"#);
        let binary = fake_envelope(1, "v", &[0, 0, 0, 0, 0, 0, 0, 3, b'a', b'b', b'c']);
        let split = binary.len() / 2;
        let mut rest = full.clone();
        rest.extend_from_slice(&binary[..split]);
        let (pending, leftover) = decode_seed(rest);
        assert_eq!(
            pending.into_iter().collect::<Vec<_>>(),
            vec![StreamItem::Text(r#"{"t":"ping"}"#.to_string())]
        );
        let leftover: Vec<u8> = leftover.into_iter().collect();
        assert_eq!(
            leftover,
            binary[..split].to_vec(),
            "the partial envelope head must seed the read buffer"
        );
    }
}
