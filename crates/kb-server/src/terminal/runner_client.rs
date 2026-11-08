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
/// terminal bytes; the runner truncates at 16 MiB, so a legal reply
/// fits with room for the envelope).
pub(crate) const MAX_REPLY_BYTES: usize = 17 * 1024 * 1024;

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

/// The per-op relay budget: a floor over the configured base timeout.
///
/// Runner durations from the merged `kb-term` code: `enter` waits up
/// to `enter_max_wait_ms` (5 s) + quiet, `kill` polls the exit-keys
/// grace (5 s default) plus two 2 s supervisory sleeps and a possible
/// 10 s systemctl stop, `launch` stacks the holder poll, anchor polls
/// and up to the 10 s tap wait, and `list` spawns ~8 tmux subprocesses
/// per session. A flat 5 s base would race successful ops and then
/// report 503 while the runner kept mutating. Input ops (paste,
/// keys) and the other single-step ops use the base; `list` gets 30 s
/// (a large session count is the slow case); `launch`/`kill`/
/// `read`/`history` get 60 s.
fn op_budget(req: &RunnerRequest, base: Duration) -> Duration {
    let floor = match req {
        RunnerRequest::Launch { .. }
        | RunnerRequest::Kill { .. }
        | RunnerRequest::Read { .. }
        | RunnerRequest::History { .. } => Duration::from_secs(60),
        RunnerRequest::Enter { .. } => Duration::from_secs(15),
        RunnerRequest::List {} => Duration::from_secs(30),
        _ => base,
    };
    floor.max(base)
}

impl UnixRunner {
    pub(crate) fn new(path: PathBuf, timeout: Duration) -> Self {
        Self { path, timeout }
    }

    /// One request per connection: connect, write one JSON line
    /// (`{"op":"<name>",...}` + `\n`), read one JSON line, close. The
    /// deadline is per-op (see [`op_budget`]): the configured
    /// `self.timeout` is the floor for input ops, and long ops get
    /// budgets that cover their runner durations, so a successful
    /// launch/kill is never raced by a 5 s client timeout while the
    /// runner keeps mutating after a 503.
    pub(crate) async fn call(&self, req: &RunnerRequest) -> Result<RunnerReply, RunnerError> {
        let line = serde_json::to_string(req).map_err(|_| RunnerError::BadReply)?;
        let budget = op_budget(req, self.timeout);
        let outcome = tokio::time::timeout(budget, self.round_trip(line)).await;
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

    async fn serve_one(socket: std::path::PathBuf, reply: String) {
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
        serve_one(sock.clone(), "{\"status\":\"ok\",\"data\":{}}".to_string()).await;
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
        // must fire. Paste rides the base budget (heavy ops have
        // per-op floors above 1s in op_budget).
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
        let err = runner
            .call(&RunnerRequest::Paste {
                id: "abcd1234".to_string(),
                viewer: "spa".to_string(),
                text: "hi".to_string(),
            })
            .await;
        assert!(matches!(err, Err(RunnerError::Timeout)), "got {err:?}");
        assert!(started.elapsed() < Duration::from_secs(5));
    }

    #[tokio::test]
    async fn oversize_reply_is_too_large() {
        let dir = tempfile::tempdir().unwrap();
        let sock = dir.path().join("runner.sock");
        let big = "x".repeat(MAX_REPLY_BYTES + 1);
        let reply = format!("{{\"status\":\"ok\",\"data\":\"{big}\"}}");
        serve_one(sock.clone(), reply).await;
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
        serve_one(sock.clone(), "not json".to_string()).await;
        let runner = UnixRunner::new(sock, Duration::from_secs(5));
        assert!(matches!(
            runner.call(&RunnerRequest::List {}).await,
            Err(RunnerError::BadReply)
        ));
    }
    /// Per-op budgets: with the default 5 s base, long ops must wait
    /// far longer than 5 s (their runner durations), input ops use the
    /// base, and an operator-raised base always wins over a floor.
    #[test]
    fn op_budget_covers_runner_durations() {
        let base = Duration::from_secs(5);
        let launch = RunnerRequest::Launch {
            harness: super::super::wire::Harness::Omp,
            argv: None,
            resume_sid: None,
            cwd: "/work/proj".to_string(),
            cols: None,
            rows: None,
            env: None,
            exit_keys: None,
        };
        assert!(op_budget(&launch, base) >= Duration::from_secs(60));
        assert!(
            op_budget(
                &RunnerRequest::Kill {
                    id: "abcd1234".to_string(),
                    grace_secs: None
                },
                base
            ) >= Duration::from_secs(60)
        );
        assert!(
            op_budget(
                &RunnerRequest::Read {
                    id: "abcd1234".to_string()
                },
                base
            ) >= Duration::from_secs(60)
        );
        assert!(
            op_budget(
                &RunnerRequest::History {
                    id: "abcd1234".to_string()
                },
                base
            ) >= Duration::from_secs(60)
        );
        assert!(
            op_budget(
                &RunnerRequest::Enter {
                    id: "abcd1234".to_string(),
                    viewer: "spa".to_string(),
                    blocked: false,
                    from_composer: true
                },
                base
            ) >= Duration::from_secs(15)
        );
        assert!(op_budget(&RunnerRequest::List {}, base) >= Duration::from_secs(30));
        assert_eq!(
            op_budget(
                &RunnerRequest::Paste {
                    id: "abcd1234".to_string(),
                    viewer: "spa".to_string(),
                    text: "x".to_string()
                },
                base
            ),
            base,
            "input ops use the base"
        );
        assert_eq!(
            op_budget(&RunnerRequest::List {}, Duration::from_secs(45)),
            Duration::from_secs(45),
            "an operator-raised base wins over the floor"
        );
    }

    /// A reply between the old 4 MiB cap and the current budget
    /// (a full 50k-line scrollback) must round-trip, not TooLarge.
    #[tokio::test]
    async fn five_mib_reply_roundtrips_within_the_budget() {
        let dir = tempfile::tempdir().unwrap();
        let sock = dir.path().join("runner.sock");
        let big = "x".repeat(5 * 1024 * 1024);
        let reply = format!("{{\"status\":\"ok\",\"data\":\"{big}\"}}");
        serve_one(sock.clone(), reply).await;
        let runner = UnixRunner::new(sock, Duration::from_secs(5));
        match runner.call(&RunnerRequest::List {}).await {
            Ok(RunnerReply::Ok { data }) => {
                assert_eq!(data.as_str().map(|s| s.len()), Some(5 * 1024 * 1024));
            }
            other => panic!("5 MiB reply must round-trip, got {other:?}"),
        }
    }
}
