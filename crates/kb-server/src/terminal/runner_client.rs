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
}
