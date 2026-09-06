//! Tiny SSE client used by `kb atlas recompute` (v0.3 X5) and
//! `kb push` (v0.4 C2). No `reqwest-eventsource` dep — both paths
//! parse the byte stream directly so the failure modes are explicit
//! (`/api/events` drops the connection vs the daemon dies vs we get
//! a partial frame on shutdown).
//!
//! Frame format (per HTML Living Standard 9.2.6 + axum's emit shape):
//!
//! ```text
//! id: <last-event-id>
//! event: <kind>
//! data: <one-line json>
//!
//! ```
//!
//! Each frame is terminated by a blank line. The parser buffers bytes,
//! splits on the blank line, and yields a Frame per chunk.
//!
//! v0.7.1 P2 — the parser is now spec-conformant where it was sloppy:
//! a blank line is `\n\n` *or* `\r\n\r\n` (a proxy may rewrite line
//! endings); multiple `data:` lines concatenate with `\n`; `:`-comment
//! lines are skipped; only one optional leading space is stripped from
//! a field value (not a full `.trim()`).

use anyhow::{Context, Result};
use bytes::Bytes;
use futures::StreamExt;

#[derive(Debug, Default, Clone)]
pub struct Frame {
    pub id: Option<String>,
    pub event: Option<String>,
    pub data: Option<String>,
}

/// Strip exactly one optional leading space from an SSE field value —
/// per the spec the rest of the value is kept verbatim (a full `.trim()`
/// would also eat a legitimate trailing space / multiple leading ones).
fn strip_one_space(s: &str) -> &str {
    s.strip_prefix(' ').unwrap_or(s)
}

impl Frame {
    fn parse(text: &str) -> Self {
        let mut id = None;
        let mut event = None;
        let mut data: Vec<String> = Vec::new();
        // `str::lines()` splits on `\n` and strips a trailing `\r`, so
        // `\r\n`-delimited frames are handled here for free.
        for line in text.lines() {
            // Comment line — the SSE spec ignores anything starting `:`.
            if line.starts_with(':') {
                continue;
            }
            if let Some(rest) = line.strip_prefix("id:") {
                id = Some(strip_one_space(rest).to_string());
            } else if let Some(rest) = line.strip_prefix("event:") {
                event = Some(strip_one_space(rest).to_string());
            } else if let Some(rest) = line.strip_prefix("data:") {
                // The spec concatenates multiple `data:` lines with `\n`.
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

/// Open `<base>/api/events`, optionally with an `Authorization: Bearer`
/// header + `Last-Event-ID` resume marker. `query` is an extra
/// pre-encoded query string (no leading `?`, e.g.
/// `types=index.*&filter=kb:docs`) — empty for the unfiltered firehose.
/// Returns the byte stream the caller drives via `next_frame`.
pub async fn open_events_stream(
    base: &str,
    bearer: Option<&str>,
    last_event_id: Option<&str>,
    query: &str,
) -> Result<reqwest::Response> {
    let client = reqwest::Client::builder()
        // The /api/events stream is long-lived; no per-request timeout.
        .build()?;
    let url = if query.is_empty() {
        format!("{base}/api/events")
    } else {
        format!("{base}/api/events?{query}")
    };
    let mut req = client.get(url).header("Accept", "text/event-stream");
    if let Some(token) = bearer {
        req = req.header("Authorization", format!("Bearer {token}"));
    }
    if let Some(eid) = last_event_id {
        req = req.header("Last-Event-ID", eid);
    }
    let resp = req
        .send()
        .await
        .with_context(|| format!("GET {base}/api/events"))?;
    Ok(resp.error_for_status()?)
}

/// Drain frames from a chunked byte stream. Buffers across chunk
/// boundaries; yields one Frame per complete blank-line-terminated event.
pub struct FrameReader {
    stream: std::pin::Pin<Box<dyn futures::Stream<Item = reqwest::Result<Bytes>> + Send + 'static>>,
    buf: Vec<u8>,
}

impl FrameReader {
    pub fn from_response(resp: reqwest::Response) -> Self {
        Self {
            stream: Box::pin(resp.bytes_stream()),
            buf: Vec::new(),
        }
    }

    /// Pull the next complete frame, or `Ok(None)` when the stream
    /// closes. Network errors propagate as `Err`.
    pub async fn next_frame(&mut self) -> Result<Option<Frame>> {
        loop {
            // First, try to drain a complete frame from what we have.
            if let Some((pos, term_len)) = find_frame_end(&self.buf) {
                let raw = self.buf.drain(..pos + term_len).collect::<Vec<_>>();
                let text = String::from_utf8_lossy(&raw);
                return Ok(Some(Frame::parse(&text)));
            }
            // Pull more bytes.
            match self.stream.next().await {
                Some(Ok(chunk)) => self.buf.extend_from_slice(&chunk),
                Some(Err(e)) => return Err(e.into()),
                None => return Ok(None),
            }
        }
    }
}

/// v0.24 T1 (operator ruling D7) — the shared SSE tail loop that both
/// `kb push` and `kb events --follow` wrap: connect, drain frames into
/// `on_frame`, reconnect on disconnect with `Last-Event-ID` resume +
/// exponential backoff (1/2/4/8/16/30s capped), and exit cleanly on
/// SIGINT/SIGTERM. Extracted from `commands::push` so the two verbs
/// share ONE loop instead of duplicating it.
pub struct TailOpts<'a> {
    /// Daemon base URL, no trailing slash (e.g. `http://127.0.0.1:4000`).
    pub base: &'a str,
    pub bearer: Option<&'a str>,
    /// Pre-encoded extra query string (no leading `?`), e.g.
    /// `types=index.*&filter=kb:docs`. Empty = the unfiltered firehose.
    pub query: &'a str,
    /// Label for stderr status lines, e.g. `kb push` / `kb events`.
    pub label: &'a str,
}

/// Backoff schedule on disconnect: 1s, 2s, 4s, 8s, then capped at 30s.
const BACKOFF_STEPS: &[u64] = &[1, 2, 4, 8, 16, 30];

/// A connection that stayed up at least this long counts as a "real"
/// session — reset the backoff on its (clean) close. v0.7.1 P2: a
/// shorter-lived `Ok(())` close (an idle long-poll the daemon or a
/// proxy dropped, or a flapping connection) keeps backing off instead,
/// so the tail loop doesn't hammer a fixed 1-req/s reconnect loop.
const STABLE_SESSION: std::time::Duration = std::time::Duration::from_secs(5);

pub async fn tail_events(opts: TailOpts<'_>, mut on_frame: impl FnMut(&Frame)) -> Result<()> {
    let label = opts.label;
    let mut last_id: Option<String> = None;
    let mut step = 0usize;
    // LOW (deep-review): tokio installs a default SIGINT handler that
    // aborts the process abruptly. That's fine here (no unflushed state)
    // but it lands mid-`tokio::time::sleep` and prints a stack-noise
    // message; install a clean shutdown path via a signal future that
    // breaks the outer loop. SIGTERM gets the same treatment on unix.
    let ctrl_c = tokio::signal::ctrl_c();
    tokio::pin!(ctrl_c);
    #[cfg(unix)]
    let mut sigterm =
        tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()).ok();
    loop {
        let resume = last_id.clone();
        let started = std::time::Instant::now();
        let outcome = tokio::select! {
            biased;
            _ = &mut ctrl_c => {
                eprintln!("[{label}] received SIGINT; exiting");
                return Ok(());
            }
            _ = async {
                #[cfg(unix)]
                { if let Some(s) = sigterm.as_mut() { s.recv().await; } else { std::future::pending::<()>().await; } }
                #[cfg(not(unix))]
                { std::future::pending::<()>().await; }
            } => {
                eprintln!("[{label}] received SIGTERM; exiting");
                return Ok(());
            }
            o = tail_once(&opts, resume.as_deref(), &mut last_id, &mut on_frame) => o,
        };
        let uptime = started.elapsed();
        let delay_secs = match outcome {
            Ok(()) if uptime >= STABLE_SESSION => {
                // A real session ended (e.g. the daemon restarted) —
                // reset backoff and reconnect promptly.
                eprintln!("[{label}] stream closed after {uptime:?}; reconnecting in 1s …");
                step = 0;
                1
            }
            Ok(()) => {
                // Closed too quickly to be a real session (idle long-poll
                // dropped, or flapping) — back off like an error.
                let delay = BACKOFF_STEPS[step.min(BACKOFF_STEPS.len() - 1)];
                eprintln!("[{label}] stream closed after {uptime:?}; retrying in {delay}s …");
                step = (step + 1).min(BACKOFF_STEPS.len() - 1);
                delay
            }
            Err(e) => {
                let delay = BACKOFF_STEPS[step.min(BACKOFF_STEPS.len() - 1)];
                eprintln!("[{label}] {e}; retrying in {delay}s …");
                step = (step + 1).min(BACKOFF_STEPS.len() - 1);
                delay
            }
        };
        // Also abort the backoff sleep if a signal fires (avoids the
        // 30s tail blocking the exit on the last iteration before exit).
        tokio::select! {
            biased;
            _ = &mut ctrl_c => {
                eprintln!("[{label}] received SIGINT during backoff; exiting");
                return Ok(());
            }
            _ = tokio::time::sleep(std::time::Duration::from_secs(delay_secs)) => {}
        }
    }
}

/// One connect-and-drain pass: every complete frame goes to `on_frame`;
/// the resume cursor advances on every frame that carries an `id:`.
async fn tail_once(
    opts: &TailOpts<'_>,
    last_event_id: Option<&str>,
    last_id: &mut Option<String>,
    on_frame: &mut impl FnMut(&Frame),
) -> Result<()> {
    let resp = open_events_stream(opts.base, opts.bearer, last_event_id, opts.query).await?;
    let mut reader = FrameReader::from_response(resp);
    while let Some(frame) = reader.next_frame().await? {
        if let Some(id) = &frame.id {
            *last_id = Some(id.clone());
        }
        on_frame(&frame);
    }
    Ok(())
}

/// Position of the blank-line frame terminator and its byte length.
/// Per the WHATWG EventSource spec, a frame ends at "a blank line" —
/// which itself can be `\n`, `\r\n`, or `\r`. So a blank line is two
/// consecutive line terminators in any combination.
///
/// LOW (deep-review): pre-fix only `\n\n` and `\r\n\r\n` were
/// recognised; `\r\r` and mixed forms (`\n\r\n`, `\r\n\n`) leaked
/// through and the parser hung waiting for a blank line. Real
/// servers + proxies tend to be consistent, but a heterogeneous
/// chunk-boundary mid-stream can split a `\r\n` across reads.
fn find_frame_end(buf: &[u8]) -> Option<(usize, usize)> {
    let mut i = 0;
    while i < buf.len() {
        // Greedy: longest match first so `\r\n\r\n` isn't read as `\r\n`+`\r\n`.
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_basic_frame() {
        let f = Frame::parse("id: 7\nevent: index.complete\ndata: {\"kb\":\"smoke\"}\n");
        assert_eq!(f.id.as_deref(), Some("7"));
        assert_eq!(f.event.as_deref(), Some("index.complete"));
        assert_eq!(f.data.as_deref(), Some("{\"kb\":\"smoke\"}"));
    }

    #[test]
    fn parse_concatenates_multiple_data_lines() {
        // The SSE spec joins repeated `data:` fields with `\n` — the
        // pre-P2 parser kept only the last line.
        let f = Frame::parse("event: x\ndata: line one\ndata: line two\n");
        assert_eq!(f.data.as_deref(), Some("line one\nline two"));
    }

    #[test]
    fn parse_skips_comment_lines_and_strips_one_space() {
        let f = Frame::parse(": keep-alive\nevent:  spaced\ndata:  {\"a\":1}\n");
        // `:`-comment ignored; only ONE leading space stripped, so the
        // value keeps its second space verbatim.
        assert_eq!(f.event.as_deref(), Some(" spaced"));
        assert_eq!(f.data.as_deref(), Some(" {\"a\":1}"));
    }

    #[test]
    fn find_frame_end_handles_lf_and_crlf() {
        assert_eq!(find_frame_end(b"data: x\n\nmore"), Some((7, 2)));
        assert_eq!(find_frame_end(b"data: x\r\n\r\nmore"), Some((7, 4)));
        // No blank line yet → None (the reader waits for more bytes).
        assert_eq!(find_frame_end(b"data: x\n"), None);
        // The leftmost terminator wins.
        assert_eq!(find_frame_end(b"a\n\nb\r\n\r\n"), Some((1, 2)));
    }

    #[tokio::test]
    async fn frame_reader_splits_crlf_terminated_stream() {
        // A proxy that rewrote line endings to CRLF must not wedge the
        // reader (pre-P2 `find_double_newline` only matched `\n\n`).
        let body = "id: 1\r\nevent: a\r\ndata: {}\r\n\r\nid: 2\r\nevent: b\r\ndata: {}\r\n\r\n";
        let stream = futures::stream::iter(vec![Ok::<Bytes, reqwest::Error>(Bytes::from(body))]);
        let mut reader = FrameReader {
            stream: Box::pin(stream),
            buf: Vec::new(),
        };
        let f1 = reader.next_frame().await.unwrap().expect("frame 1");
        assert_eq!(f1.id.as_deref(), Some("1"));
        assert_eq!(f1.event.as_deref(), Some("a"));
        let f2 = reader.next_frame().await.unwrap().expect("frame 2");
        assert_eq!(f2.id.as_deref(), Some("2"));
        assert!(reader.next_frame().await.unwrap().is_none(), "stream end");
    }
}
