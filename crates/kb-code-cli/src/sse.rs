//! Minimal SSE tail for `kb-code events --follow` — a trimmed, self-
//! contained port of kb-cli's own frame parser
//! (`crates/kb-cli/src/sse.rs`). kb-code-cli can't depend on kb-cli
//! directly to reuse it: kb-cli ships bin-only (no `[lib]` target), so
//! there's nothing to import.
//!
//! Differs from kb-cli's version in one deliberate way: this reconnects on
//! disconnect with a flat 2s delay rather than kb's fuller
//! 1/2/4/8/16/30s-capped exponential backoff schedule. kb-code's daemon is
//! always local (Wave-1 has no remote-daemon story), so a short flat delay
//! is enough; port the fuller schedule here if that ever changes.

use anyhow::{Context, Result};
use futures::stream::BoxStream;
use futures::StreamExt;

#[derive(Debug, Default, Clone)]
pub struct Frame {
    pub id: Option<String>,
    pub event: Option<String>,
    pub data: Option<String>,
}

/// Strip exactly one optional leading space from an SSE field value — per
/// the spec the rest of the value is kept verbatim.
fn strip_one_space(s: &str) -> &str {
    s.strip_prefix(' ').unwrap_or(s)
}

impl Frame {
    fn parse(text: &str) -> Self {
        let mut id = None;
        let mut event = None;
        let mut data: Vec<String> = Vec::new();
        for line in text.lines() {
            if line.starts_with(':') {
                continue; // SSE comment line
            }
            if let Some(rest) = line.strip_prefix("id:") {
                id = Some(strip_one_space(rest).to_string());
            } else if let Some(rest) = line.strip_prefix("event:") {
                event = Some(strip_one_space(rest).to_string());
            } else if let Some(rest) = line.strip_prefix("data:") {
                data.push(strip_one_space(rest).to_string());
            }
        }
        Frame {
            id,
            event,
            data: (!data.is_empty()).then(|| data.join("\n")),
        }
    }
}

/// Position + byte length of the blank-line frame terminator — a blank line
/// can be `\n\n`, `\r\n\r\n`, or a mixed/short form if a proxy rewrote line
/// endings mid-stream (same set kb-cli's own parser handles).
fn find_frame_end(buf: &[u8]) -> Option<(usize, usize)> {
    let mut i = 0;
    while i < buf.len() {
        if buf[i..].starts_with(b"\r\n\r\n") {
            return Some((i, 4));
        }
        if buf[i..].starts_with(b"\n\r\n") || buf[i..].starts_with(b"\r\n\n") {
            return Some((i, 3));
        }
        if buf[i..].starts_with(b"\n\n") || buf[i..].starts_with(b"\r\r") {
            return Some((i, 2));
        }
        i += 1;
    }
    None
}

const RECONNECT_DELAY: std::time::Duration = std::time::Duration::from_secs(2);

/// One connected `/api/events` session. Pull-based so a caller can do
/// async work (a refetch) between frames — `tail`'s sync `on_frame`
/// callback cannot. `annotate watch` is the motivating consumer.
pub struct SseSession {
    stream: BoxStream<'static, reqwest::Result<Vec<u8>>>,
    buf: Vec<u8>,
    last_id: Option<String>,
}

impl SseSession {
    /// Open `<base>/api/events`, optionally resuming from `Last-Event-ID`.
    /// No request timeout — this is a long-lived stream.
    pub async fn connect(base: &str, last_event_id: Option<&str>) -> Result<Self> {
        // V70-A2 — the shared builder (default `X-Kbc-Request: 1`).
        let client = crate::client_builder().build()?;
        let url = format!("{}/api/events", base.trim_end_matches('/'));
        let mut req = client.get(&url).header("Accept", "text/event-stream");
        if let Some(id) = last_event_id {
            req = req.header("Last-Event-ID", id);
        }
        let resp = req
            .send()
            .await
            .with_context(|| format!("GET {url}"))?
            .error_for_status()
            .with_context(|| format!("GET {url}"))?;
        Ok(Self {
            stream: Box::pin(resp.bytes_stream().map(|r| r.map(|b| b.to_vec()))),
            buf: Vec::new(),
            last_id: last_event_id.map(str::to_string),
        })
    }

    /// Next complete frame, or `Ok(None)` when the stream ends cleanly.
    pub async fn next_frame(&mut self) -> Result<Option<Frame>> {
        loop {
            if let Some((pos, term)) = find_frame_end(&self.buf) {
                let raw: Vec<u8> = self.buf.drain(..pos + term).collect();
                let text = String::from_utf8_lossy(&raw);
                let frame = Frame::parse(&text);
                if let Some(id) = &frame.id {
                    self.last_id = Some(id.clone());
                }
                return Ok(Some(frame));
            }
            match self.stream.next().await {
                Some(Ok(chunk)) => self.buf.extend_from_slice(&chunk),
                Some(Err(e)) => return Err(e.into()),
                None => return Ok(None),
            }
        }
    }

    pub fn last_id(&self) -> Option<&str> {
        self.last_id.as_deref()
    }
}

/// Connect to `<daemon>/api/events` and call `on_frame` for every complete
/// frame, forever — reconnecting (with `Last-Event-ID` resume) on
/// disconnect until SIGINT.
pub async fn tail(daemon: &str, on_frame: impl Fn(&Frame)) -> Result<()> {
    let base = daemon.trim_end_matches('/').to_string();
    let mut last_id: Option<String> = None;
    let ctrl_c = tokio::signal::ctrl_c();
    tokio::pin!(ctrl_c);
    loop {
        let resume = last_id.clone();
        let outcome = tokio::select! {
            biased;
            _ = &mut ctrl_c => return Ok(()),
            o = tail_once(&base, resume.as_deref(), &on_frame) => o,
        };
        match outcome {
            Ok(new_last) => {
                last_id = new_last;
                eprintln!("[kb-code events] stream closed; reconnecting in 2s …");
            }
            Err(e) => eprintln!("[kb-code events] {e}; retrying in 2s …"),
        }
        tokio::select! {
            biased;
            _ = &mut ctrl_c => return Ok(()),
            _ = tokio::time::sleep(RECONNECT_DELAY) => {}
        }
    }
}

/// One connect-and-drain pass. Returns the last-seen event id (for the next
/// reconnect's resume cursor) once the stream closes cleanly.
async fn tail_once(
    base: &str,
    last_event_id: Option<&str>,
    on_frame: &impl Fn(&Frame),
) -> Result<Option<String>> {
    let mut session = SseSession::connect(base, last_event_id).await?;
    while let Some(frame) = session.next_frame().await? {
        on_frame(&frame);
    }
    Ok(session.last_id().map(str::to_string))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_basic_frame() {
        let f = Frame::parse("id: 7\nevent: mirror.updated\ndata: {\"repo\":\"kb\"}\n");
        assert_eq!(f.id.as_deref(), Some("7"));
        assert_eq!(f.event.as_deref(), Some("mirror.updated"));
        assert_eq!(f.data.as_deref(), Some("{\"repo\":\"kb\"}"));
    }

    #[test]
    fn parse_concatenates_multiple_data_lines() {
        let f = Frame::parse("event: x\ndata: line one\ndata: line two\n");
        assert_eq!(f.data.as_deref(), Some("line one\nline two"));
    }

    #[test]
    fn parse_skips_comment_lines_and_strips_one_space() {
        let f = Frame::parse(": keep-alive\nevent:  spaced\ndata:  {\"a\":1}\n");
        assert_eq!(f.event.as_deref(), Some(" spaced"));
        assert_eq!(f.data.as_deref(), Some(" {\"a\":1}"));
    }

    #[test]
    fn find_frame_end_handles_lf_and_crlf() {
        assert_eq!(find_frame_end(b"data: x\n\nmore"), Some((7, 2)));
        assert_eq!(find_frame_end(b"data: x\r\n\r\nmore"), Some((7, 4)));
        assert_eq!(find_frame_end(b"data: x\n"), None);
    }

    #[tokio::test]
    async fn tail_once_yields_frames_and_tracks_last_id() {
        // Exercises the parser end-to-end over a fake two-frame body via a
        // local HTTP server, rather than re-deriving FrameReader's own
        // logic in a unit test — the fixture matches
        // `crates/kb-cli/src/sse.rs`'s own `frame_reader_splits_crlf_
        // terminated_stream` test shape.
        use std::sync::{Arc, Mutex};

        let body =
            "id: 1\r\nevent: mirror.updated\r\ndata: {}\r\n\r\nid: 2\r\nevent: repo.head_moved\r\ndata: {}\r\n\r\n";
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        listener.set_nonblocking(true).unwrap();
        let listener = tokio::net::TcpListener::from_std(listener).unwrap();
        tokio::spawn(async move {
            use tokio::io::{AsyncReadExt, AsyncWriteExt};
            let (mut sock, _) = listener.accept().await.unwrap();
            let mut buf = [0u8; 1024];
            let _ = sock.read(&mut buf).await;
            let resp = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\n\r\n{}",
                body.len(),
                body
            );
            let _ = sock.write_all(resp.as_bytes()).await;
        });

        let seen: Arc<Mutex<Vec<Frame>>> = Arc::new(Mutex::new(Vec::new()));
        let seen2 = seen.clone();
        let last = tail_once(&format!("http://{addr}"), None, &move |f: &Frame| {
            seen2.lock().unwrap().push(f.clone());
        })
        .await
        .unwrap();

        assert_eq!(last.as_deref(), Some("2"));
        let frames = seen.lock().unwrap();
        assert_eq!(frames.len(), 2);
        assert_eq!(frames[0].event.as_deref(), Some("mirror.updated"));
        assert_eq!(frames[1].event.as_deref(), Some("repo.head_moved"));
    }
}
