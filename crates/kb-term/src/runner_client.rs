//! The operator-local runner client: newline-delimited JSON over
//! `runner.sock` (`runner/server.rs` framing, `{v:1, id, op, ...}` /
//! `{v:1, id, status: "ok"|"err", ...}`), backed by the wire types in
//! [`crate::wire`].
//!
//! Resolution order for the runner socket:
//! the `--runner-sock` flag, then `$KB_TERM_RUNNER_SOCK`, then
//! `$KB_TERM_RUN_DIR` joined with `/runner/runner.sock`, and finally
//! the default `/run/kb-term/runner/runner.sock`.
//!
//! The tap socket for a session derives from the same run dir:
//! `<run-dir>/s/<id>/tap.sock` (mirroring `tap/src/viewers.ts:194`).

use std::path::{Path, PathBuf};
use std::time::Duration;

use tokio::io::{AsyncReadExt, AsyncWriteExt};

use crate::wire::{Request, Response};

/// The default runner socket (when no override is given).
pub const DEFAULT_RUNNER_SOCKET: &str = "/run/kb-term/runner/runner.sock";
/// The env var naming the runner socket.
pub const RUNNER_SOCK_ENV: &str = "KB_TERM_RUNNER_SOCK";
/// The env var naming the run dir (shared with `kb-termd`).
pub const RUN_DIR_ENV: &str = "KB_TERM_RUN_DIR";

/// Resolve the runner socket path: flag > env socket > run-dir env >
/// the default.
#[must_use]
pub fn resolve_runner_socket(flag: Option<&str>) -> PathBuf {
    // An empty flag (or env var) never becomes an empty path: fall
    // through to the next source.
    if let Some(p) = flag.filter(|p| !p.is_empty()) {
        return PathBuf::from(p);
    }
    if let Ok(p) = std::env::var(RUNNER_SOCK_ENV) {
        if !p.is_empty() {
            return PathBuf::from(p);
        }
    }
    if let Ok(d) = std::env::var(RUN_DIR_ENV) {
        if !d.is_empty() {
            return PathBuf::from(d).join("runner").join("runner.sock");
        }
    }
    PathBuf::from(DEFAULT_RUNNER_SOCKET)
}

/// The run dir for a runner socket path: its parent's parent (the
/// socket lives at `<run-dir>/runner/runner.sock`). For any other
/// shape this is still the ancestor two levels up — the derivation
/// is mechanical and the fake runner/tap in tests use the same.
#[must_use]
pub fn run_dir_of(runner_sock: &Path) -> PathBuf {
    runner_sock
        .parent()
        .and_then(|p| p.parent())
        .map(Path::to_path_buf)
        .unwrap_or_else(|| PathBuf::from("/run/kb-term"))
}

/// The tap socket path for a session id (mirroring
/// `tap/src/viewers.ts:194`: `<runDir>/s/<session>/tap.sock`).
#[must_use]
pub fn tap_socket_path(run_dir: &Path, session: &str) -> PathBuf {
    run_dir.join("s").join(session).join("tap.sock")
}

/// Errors from the runner client. The `Unreachable` variant means the
/// socket could not be opened, written, or read (the runner daemon is
/// down); `Code` carries a runner error code (e.g. `grid-owned`,
/// `no-session`, `invalid-id`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RunnerClientError {
    /// The socket could not be reached (or no response from it).
    Unreachable(String),
    /// The runner answered with an error code.
    Code { code: String, message: String },
}

impl RunnerClientError {
    /// The runner error code, if this is a `Code` error.
    #[must_use]
    pub fn code(&self) -> Option<&str> {
        match self {
            RunnerClientError::Code { code, .. } => Some(code),
            RunnerClientError::Unreachable(_) => None,
        }
    }
}

impl std::fmt::Display for RunnerClientError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            RunnerClientError::Unreachable(m) => write!(f, "{m}"),
            RunnerClientError::Code { code, message } => write!(f, "{code}: {message}"),
        }
    }
}

impl std::error::Error for RunnerClientError {}

/// An RPC over runner.sock. One connection per `RunnerClient`, kept
/// open for many requests (the server answers in request order).
///
/// Replies are paired to calls by the id echoed in the reply line, and an
/// id-carrying op's echoed id is its SESSION id, so two calls on one
/// session carry the same id. A reply that arrives late (after the
/// per-call deadline) would therefore be indistinguishable from the answer
/// to the NEXT call on that session. So any call that fails after its
/// request may have reached the runner (a timeout, a read or write
/// error, a reply that does not pair or parse) drops the connection; the
/// next call dials a fresh one, whose reply stream cannot hold a stale
/// line. The same holds when the call future is DROPPED mid-call (the
/// connection is owned by the call while a request is in flight, not
/// parked in `self`). A runner error reply (`Code`) is a complete, paired
/// answer and leaves the connection in place.
pub struct RunnerClient {
    /// The live connection; `None` after a failed call (re-dialled by the
    /// next call).
    stream: Option<tokio::net::UnixStream>,
    next_id: u64,
    /// The socket path (for error messages and reconnects).
    path: PathBuf,
    /// Per-call deadline.
    timeout: Duration,
}

impl RunnerClient {
    /// Connect to `path`.
    pub async fn connect(path: &Path) -> Result<Self, RunnerClientError> {
        let stream = dial(path).await?;
        Ok(Self {
            stream: Some(stream),
            next_id: 1,
            path: path.to_path_buf(),
            timeout: Duration::from_secs(10),
        })
    }

    /// Send one request, return the response payload `data`, or an
    /// error naming the runner's error code.
    pub async fn call(&mut self, req: &Request) -> Result<serde_json::Value, RunnerClientError> {
        let id = self.next_id;
        self.next_id += 1;
        let (mut line, expected) = request_line(id, req).map_err(RunnerClientError::Unreachable)?;
        line.push('\n');
        // Cancel-safe: the connection is TAKEN out of `self` for the whole
        // exchange and put back only after a clean, paired answer. A call
        // future dropped at any await (after the request was written,
        // before its reply was read) drops the connection with it, so the
        // next call dials a fresh one and can never read this call's reply.
        let mut stream = match self.stream.take() {
            Some(stream) => stream,
            None => dial(&self.path).await?,
        };
        let outcome = Self::exchange(
            &mut stream,
            &self.path,
            self.timeout,
            line.as_bytes(),
            &expected,
        )
        .await;
        if !matches!(outcome, Err(RunnerClientError::Unreachable(_))) {
            // A reply (or a runner error reply) paired to this request:
            // nothing is in flight, the connection can serve the next call.
            self.stream = Some(stream);
        }
        outcome
    }

    async fn exchange(
        stream: &mut tokio::net::UnixStream,
        path: &Path,
        timeout: Duration,
        line: &[u8],
        expected: &serde_json::Value,
    ) -> Result<serde_json::Value, RunnerClientError> {
        let path = path.display().to_string();
        stream.write_all(line).await.map_err(|_| {
            RunnerClientError::Unreachable(format!("write to runner socket {path} failed"))
        })?;
        let body = tokio::time::timeout(timeout, read_response_line(stream))
            .await
            .map_err(|_| {
                RunnerClientError::Unreachable(format!(
                    "runner socket {path} did not answer in time"
                ))
            })?
            .map_err(|_| {
                RunnerClientError::Unreachable(format!("cannot read runner socket {path}"))
            })?;
        // The reply must echo the envelope id we sent: the numeric request
        // id, or for an id-carrying op the op's own session id (the server
        // echoes whatever `id` the line carried). A reply that does not
        // pair is a protocol failure, and the caller drops the connection
        // so a late line can never be taken for a later call's answer.
        match serde_json::from_str::<serde_json::Value>(&body)
            .ok()
            .and_then(|v| v.get("id").cloned())
        {
            Some(r) if r == *expected => {}
            Some(serde_json::Value::Null) | None => {
                return Err(RunnerClientError::Unreachable(
                    "runner reply has no id".to_string(),
                ));
            }
            Some(_) => {
                return Err(RunnerClientError::Unreachable(
                    "runner socket answered an unexpected id".to_string(),
                ));
            }
        }
        match serde_json::from_str::<Response>(&body) {
            Ok(Response::Ok { data }) => Ok(data),
            Ok(Response::Err { code, message }) => Err(RunnerClientError::Code { code, message }),
            Err(e) => Err(RunnerClientError::Unreachable(format!(
                "bad runner reply: {e}"
            ))),
        }
    }
}

async fn dial(path: &Path) -> Result<tokio::net::UnixStream, RunnerClientError> {
    tokio::net::UnixStream::connect(path).await.map_err(|e| {
        RunnerClientError::Unreachable(format!(
            "cannot connect to runner socket {}: {e}",
            path.display()
        ))
    })
}

/// Read one response line, bounded by this client's own 64 KiB response-line cap.
/// Never reads past the newline, so the next response stays intact.
async fn read_response_line(stream: &mut tokio::net::UnixStream) -> std::io::Result<String> {
    const MAX_LINE: usize = 64 * 1024;
    let mut line = Vec::new();
    let mut chunk = [0u8; 4096];
    loop {
        let n = stream.read(&mut chunk).await?;
        if n == 0 {
            return Ok(String::from_utf8_lossy(&line).into_owned());
        }
        let mut done = false;
        for &b in &chunk[..n] {
            line.push(b);
            if b == b'\n' {
                done = true;
                break;
            }
            if line.len() > MAX_LINE {
                return Err(std::io::Error::from(std::io::ErrorKind::InvalidData));
            }
        }
        if done {
            return Ok(String::from_utf8_lossy(&line).into_owned());
        }
    }
}

/// Build the request line for a wire request and the `id` its reply must
/// echo.
///
/// The envelope is `{"v":1,"id":n,"op":...}` with a NUMERIC request id,
/// but most ops carry their own STRING session id in a field also named
/// `id` (kill, resize, stream, viewer_open, keys, ...). The two share one
/// JSON key, so the rule is: an op that already has a string `id` keeps it
/// (the runner's `parse_request` treats a string `id` as the session id)
/// and the numeric envelope id is NOT written; the server echoes the
/// line's `id`, so the reply is then correlated on the session id. An op
/// without an `id` (ping, list, launch) gets the numeric envelope id as
/// before, so an old runner and a new client still agree on those.
fn request_line(id: u64, req: &Request) -> Result<(String, serde_json::Value), String> {
    let mut v = serde_json::to_value(req).map_err(|e| format!("encode request: {e}"))?;
    let obj = v
        .as_object_mut()
        .ok_or_else(|| "request is not an object".to_string())?;
    obj.insert("v".to_string(), serde_json::json!(1));
    let expected = match obj.get("id") {
        Some(sid) if sid.is_string() => sid.clone(),
        _ => {
            let n = serde_json::json!(id);
            obj.insert("id".to_string(), n.clone());
            n
        }
    };
    let line = serde_json::to_string(&v).map_err(|e| format!("encode request: {e}"))?;
    Ok((line, expected))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::wire::Harness;

    /// Guards process env: restores the previous values on drop, and
    /// serializes env-mutating tests so they cannot race each other.
    struct EnvGuard {
        saved: Vec<(&'static str, Option<String>)>,
    }
    static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
    impl EnvGuard {
        fn take(keys: &[&'static str]) -> (std::sync::MutexGuard<'static, ()>, EnvGuard) {
            let lock = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
            let saved = keys.iter().map(|k| (*k, std::env::var(k).ok())).collect();
            (lock, EnvGuard { saved })
        }
    }
    impl Drop for EnvGuard {
        fn drop(&mut self) {
            for (key, old) in self.saved.drain(..) {
                match old {
                    Some(v) => std::env::set_var(key, v),
                    None => std::env::remove_var(key),
                }
            }
        }
    }

    #[test]
    fn runner_socket_resolution_order() {
        // The env fallback order is verified against the live env:
        // set/unset each and check the precedence.
        let (_lock, _guard) = EnvGuard::take(&[RUNNER_SOCK_ENV, RUN_DIR_ENV]);
        std::env::remove_var(RUNNER_SOCK_ENV);
        std::env::remove_var(RUN_DIR_ENV);
        assert_eq!(
            resolve_runner_socket(None),
            PathBuf::from(DEFAULT_RUNNER_SOCKET)
        );
        // An empty flag falls through (never an empty path).
        assert_eq!(
            resolve_runner_socket(Some("")),
            PathBuf::from(DEFAULT_RUNNER_SOCKET)
        );
        std::env::set_var(RUN_DIR_ENV, "/tmp/rundir");
        assert_eq!(
            resolve_runner_socket(None),
            PathBuf::from("/tmp/rundir/runner/runner.sock")
        );
        std::env::set_var(RUNNER_SOCK_ENV, "/tmp/sock.sock");
        assert_eq!(
            resolve_runner_socket(None),
            PathBuf::from("/tmp/sock.sock"),
            "the socket env beats the run-dir env"
        );
        assert_eq!(
            resolve_runner_socket(Some("/tmp/flag.sock")),
            PathBuf::from("/tmp/flag.sock"),
            "the flag beats both envs"
        );
    }

    #[test]
    fn run_dir_and_tap_paths() {
        let sock = Path::new("/run/kb-term/runner/runner.sock");
        assert_eq!(run_dir_of(sock), PathBuf::from("/run/kb-term"));
        assert_eq!(
            tap_socket_path(&run_dir_of(sock), "abcd1234"),
            PathBuf::from("/run/kb-term/s/abcd1234/tap.sock")
        );
    }

    #[test]
    fn request_line_envelope() {
        let (line, expected) = request_line(7, &Request::Ping {}).unwrap();
        assert_eq!(expected, 7);
        let v: serde_json::Value = serde_json::from_str(&line).unwrap();
        assert_eq!(v["v"], 1);
        assert_eq!(v["id"], 7);
        assert_eq!(v["op"], "ping");
        let (line, _) = request_line(
            3,
            &Request::Launch {
                harness: Harness::Omp,
                argv: Some(vec!["/usr/local/bin/omp".to_string()]),
                resume_sid: None,
                cwd: "/tmp".to_string(),
                cols: None,
                rows: None,
                env: None,
                exit_keys: None,
            },
        )
        .unwrap();
        let v: serde_json::Value = serde_json::from_str(&line).unwrap();
        assert_eq!(v["harness"], "omp");
        assert_eq!(v["argv"][0], "/usr/local/bin/omp");
    }

    #[test]
    fn id_carrying_op_keeps_its_session_id_and_is_correlated_on_it() {
        let (line, expected) = request_line(
            9,
            &Request::ViewerOpen {
                id: "abcd1234".to_string(),
                viewer: "v1".to_string(),
            },
        )
        .unwrap();
        let v: serde_json::Value = serde_json::from_str(&line).unwrap();
        assert_eq!(
            v["id"], "abcd1234",
            "the numeric envelope id must not overwrite it"
        );
        assert_eq!(v["viewer"], "v1");
        assert_eq!(expected, "abcd1234");
    }

    /// End to end over a real `runner.sock` server (real dispatch, no tmux:
    /// the session does not exist, so each op answers `no-session` NAMING the
    /// session id it received). Before the fix the client overwrote the
    /// session id with the numeric envelope id, the runner stripped it, and
    /// every one of these answered `bad-request` "missing field `id`".
    #[tokio::test]
    async fn id_carrying_ops_reach_dispatch_with_their_session_id_over_the_socket() {
        use crate::runner::config::RunnerConfig;
        use crate::runner::server::{bind_runner_socket, serve};
        use crate::runner::Runner;
        use std::sync::Arc;

        let dir = tempfile::tempdir().unwrap();
        let cfg = RunnerConfig::test_config(
            dir.path().to_path_buf(),
            "/bin/true".to_string(),
            PathBuf::from("/dev/null"),
        );
        let runner = Arc::new(Runner::direct(cfg));
        let sock = runner.cfg.run_dir.join("runner").join("runner.sock");
        std::fs::create_dir_all(sock.parent().unwrap()).unwrap();
        let listener = bind_runner_socket(&sock).unwrap();
        let (tx, rx) = tokio::sync::broadcast::channel::<()>(1);
        let server = tokio::spawn({
            let runner = runner.clone();
            let sock = sock.clone();
            async move {
                let _ = serve(runner, listener, sock, rx).await;
            }
        });

        let mut client = RunnerClient::connect(&sock).await.unwrap();
        // A numeric-envelope op still correlates on the same connection.
        assert!(client.call(&Request::Ping {}).await.is_ok());
        let reqs = [
            Request::ViewerOpen {
                id: "abcd1234".to_string(),
                viewer: "v1".to_string(),
            },
            Request::ViewerClose {
                id: "abcd1234".to_string(),
                viewer: "v1".to_string(),
            },
            Request::TakeSize {
                id: "abcd1234".to_string(),
                cols: 80,
                rows: 24,
                owner: "attach-test".to_string(),
                force: false,
            },
        ];
        for req in &reqs {
            match client.call(req).await {
                Err(RunnerClientError::Code { code, message }) => {
                    assert_eq!(code, "no-session", "{req:?}: {message}");
                    assert!(message.contains("abcd1234"), "{req:?}: {message}");
                }
                other => panic!("{req:?}: expected no-session, got {other:?}"),
            }
        }
        // The connection is still correlated after id-carrying ops.
        assert!(client.call(&Request::Ping {}).await.is_ok());
        let _ = tx.send(());
        let _ = tokio::time::timeout(Duration::from_secs(10), server).await;
    }

    // ---- FXIDS: a reply that does not pair never poisons a later call ----

    use serde_json::{json, Value};
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;
    use tokio::io::AsyncBufReadExt;

    /// What the scripted runner does with one request line.
    enum Act {
        Reply(String),
        Delayed(Duration, String),
        /// Close the connection without answering.
        Close,
        /// Answer, then close the connection.
        ReplyThenClose(String),
    }

    /// The `id` a request line carries (a session-id string or the numeric
    /// envelope id), echoed the way the real server echoes it.
    fn line_id(line: &str) -> Value {
        serde_json::from_str::<Value>(line)
            .ok()
            .and_then(|v| v.get("id").cloned())
            .unwrap_or(Value::Null)
    }

    fn ok_line(id: &Value, n: u64) -> String {
        format!(
            "{}\n",
            json!({"v": 1, "id": id, "status": "ok", "data": {"n": n}})
        )
    }

    /// A scripted runner.sock: `script(connection number from 1, request
    /// line)` decides each answer. Returns the accept count.
    fn scripted_runner(
        dir: &Path,
        script: impl Fn(usize, &str) -> Act + Send + Sync + 'static,
    ) -> (PathBuf, Arc<AtomicUsize>, tokio::task::JoinHandle<()>) {
        let sock = dir.join("scripted.sock");
        let listener = tokio::net::UnixListener::bind(&sock).unwrap();
        let conns = Arc::new(AtomicUsize::new(0));
        let script = Arc::new(script);
        let accepted = conns.clone();
        let task = tokio::spawn(async move {
            loop {
                let Ok((conn, _)) = listener.accept().await else {
                    return;
                };
                let n = accepted.fetch_add(1, Ordering::SeqCst) + 1;
                let script = script.clone();
                tokio::spawn(async move {
                    let (read, mut write) = conn.into_split();
                    let mut read = tokio::io::BufReader::new(read);
                    loop {
                        let mut line = String::new();
                        match read.read_line(&mut line).await {
                            Ok(0) | Err(_) => return,
                            Ok(_) => {}
                        }
                        let mut close_after = false;
                        let reply = match script(n, &line) {
                            Act::Close => return,
                            Act::ReplyThenClose(r) => {
                                close_after = true;
                                r
                            }
                            Act::Reply(r) => r,
                            Act::Delayed(d, r) => {
                                tokio::time::sleep(d).await;
                                r
                            }
                        };
                        if write.write_all(reply.as_bytes()).await.is_err() || close_after {
                            return;
                        }
                    }
                });
            }
        });
        (sock, conns, task)
    }

    fn grid(id: &str) -> Request {
        Request::Grid { id: id.to_string() }
    }

    /// Reply 1 arrives AFTER the client gave up on call 1. Both calls name
    /// the same session, so both replies echo the same `id`: before the fix
    /// the next call read the stale reply off the same connection and
    /// returned reply 1 as the answer to call 2.
    #[tokio::test]
    async fn a_late_reply_is_never_taken_for_the_next_calls_answer() {
        let dir = tempfile::tempdir().unwrap();
        let (sock, conns, task) = scripted_runner(dir.path(), |conn, line| {
            let id = line_id(line);
            if conn == 1 {
                Act::Delayed(Duration::from_millis(300), ok_line(&id, 1))
            } else {
                Act::Reply(ok_line(&id, 2))
            }
        });
        let mut client = RunnerClient::connect(&sock).await.unwrap();
        client.timeout = Duration::from_millis(150);
        let first = client.call(&grid("abcd1234")).await;
        assert!(
            matches!(&first, Err(RunnerClientError::Unreachable(m)) if m.contains("in time")),
            "call 1 must time out: {first:?}"
        );
        // Let the stale reply reach the dead connection's socket buffer.
        tokio::time::sleep(Duration::from_millis(450)).await;
        let second = client.call(&grid("abcd1234")).await.unwrap();
        assert_eq!(
            second["n"], 2,
            "call 2 must get its own reply, not reply 1: {second}"
        );
        assert_eq!(
            conns.load(Ordering::SeqCst),
            2,
            "the timeout dropped the connection; call 2 dialled a fresh one"
        );
        task.abort();
    }

    /// A reply naming some other id, and a reply with a null id, are each a
    /// protocol failure that drops the connection (the next call re-dials
    /// and is answered normally).
    #[tokio::test]
    async fn a_mismatched_or_null_id_fails_the_call_and_drops_the_connection() {
        for (label, bad_id, want) in [
            ("mismatched", json!("zzzz9999"), "unexpected id"),
            ("null", Value::Null, "no id"),
        ] {
            let dir = tempfile::tempdir().unwrap();
            let bad = ok_line(&bad_id, 1);
            let (sock, conns, task) = scripted_runner(dir.path(), move |conn, line| {
                if conn == 1 {
                    Act::Reply(bad.clone())
                } else {
                    Act::Reply(ok_line(&line_id(line), 2))
                }
            });
            let mut client = RunnerClient::connect(&sock).await.unwrap();
            let first = client.call(&grid("abcd1234")).await;
            assert!(
                matches!(&first, Err(RunnerClientError::Unreachable(m)) if m.contains(want)),
                "{label}: {first:?}"
            );
            let second = client.call(&grid("abcd1234")).await.unwrap();
            assert_eq!(second["n"], 2, "{label}: the next call is paired afresh");
            assert_eq!(conns.load(Ordering::SeqCst), 2, "{label}: re-dialled");
            task.abort();
        }
    }

    /// A runner error reply is a complete, paired answer: the connection
    /// stays (no needless re-dial).
    #[tokio::test]
    async fn a_runner_error_reply_keeps_the_connection() {
        let dir = tempfile::tempdir().unwrap();
        let (sock, conns, task) = scripted_runner(dir.path(), |_, line| {
            let err = json!({"v": 1, "id": line_id(line), "status": "err",
                "code": "no-session", "message": "no session abcd1234"});
            Act::Reply(format!("{err}\n"))
        });
        let mut client = RunnerClient::connect(&sock).await.unwrap();
        for _ in 0..2 {
            let e = client.call(&grid("abcd1234")).await.unwrap_err();
            assert_eq!(e.code(), Some("no-session"));
        }
        assert_eq!(conns.load(Ordering::SeqCst), 1, "one connection throughout");
        task.abort();
    }

    /// A call future dropped after its request was written (the caller's
    /// own timeout, a `select!` arm losing) must not leave the request in
    /// flight on a live connection: the next call would read ITS reply.
    #[tokio::test]
    async fn a_call_dropped_mid_flight_never_poisons_the_next_call() {
        let dir = tempfile::tempdir().unwrap();
        let (sock, conns, task) = scripted_runner(dir.path(), |conn, line| {
            let id = line_id(line);
            if conn == 1 {
                Act::Delayed(Duration::from_millis(300), ok_line(&id, 1))
            } else {
                Act::Reply(ok_line(&id, 2))
            }
        });
        let mut client = RunnerClient::connect(&sock).await.unwrap();
        let dropped =
            tokio::time::timeout(Duration::from_millis(100), client.call(&grid("abcd1234"))).await;
        assert!(dropped.is_err(), "call 1 was cancelled mid-flight");
        // Let the stale reply reach the abandoned connection.
        tokio::time::sleep(Duration::from_millis(400)).await;
        let second = client.call(&grid("abcd1234")).await.unwrap();
        assert_eq!(second["n"], 2, "call 2 gets its own reply: {second}");
        assert_eq!(conns.load(Ordering::SeqCst), 2, "a fresh connection");
        task.abort();
    }

    /// The runner closing the connection without answering (EOF) fails the
    /// call and the next call re-dials.
    #[tokio::test]
    async fn an_eof_instead_of_a_reply_fails_the_call_and_the_next_call_redials() {
        let dir = tempfile::tempdir().unwrap();
        let (sock, conns, task) = scripted_runner(dir.path(), |conn, line| {
            if conn == 1 {
                Act::Close
            } else {
                Act::Reply(ok_line(&line_id(line), 2))
            }
        });
        let mut client = RunnerClient::connect(&sock).await.unwrap();
        let first = client.call(&grid("abcd1234")).await;
        assert!(
            matches!(&first, Err(RunnerClientError::Unreachable(_))),
            "EOF is an unreachable runner: {first:?}"
        );
        let second = client.call(&grid("abcd1234")).await.unwrap();
        assert_eq!(second["n"], 2);
        assert_eq!(conns.load(Ordering::SeqCst), 2, "re-dialled after EOF");
        task.abort();
    }

    /// A write onto a connection the runner already closed fails the call
    /// (write error), and the next call re-dials.
    #[tokio::test]
    async fn a_write_error_fails_the_call_and_the_next_call_redials() {
        let dir = tempfile::tempdir().unwrap();
        let (sock, conns, task) = scripted_runner(dir.path(), |conn, line| {
            let reply = ok_line(&line_id(line), conn as u64);
            if conn == 1 {
                Act::ReplyThenClose(reply)
            } else {
                Act::Reply(reply)
            }
        });
        let mut client = RunnerClient::connect(&sock).await.unwrap();
        assert_eq!(client.call(&grid("abcd1234")).await.unwrap()["n"], 1);
        // The runner closed that connection after answering.
        tokio::time::sleep(Duration::from_millis(150)).await;
        let second = client.call(&grid("abcd1234")).await;
        assert!(
            matches!(&second, Err(RunnerClientError::Unreachable(m)) if m.contains("write")),
            "a write onto a closed connection is a write error: {second:?}"
        );
        let third = client.call(&grid("abcd1234")).await.unwrap();
        assert_eq!(third["n"], 2, "the third call is answered on a fresh dial");
        assert_eq!(conns.load(Ordering::SeqCst), 2);
        task.abort();
    }

    // ---- FXIDS: the success paths over the real runner server ----

    /// A fake tmux: `attach-session` is a raw-mode byte recorder (the
    /// hidden client's pty slave), the pane reports dead with status 0, and
    /// every other call succeeds with an empty answer.
    fn fake_tmux(dir: &Path) -> (String, PathBuf, PathBuf) {
        use std::os::unix::fs::PermissionsExt;
        let out = dir.join("typed.bin");
        let ready = dir.join("ready");
        let script = dir.join("fake_tmux.sh");
        std::fs::write(
            &script,
            format!(
                "#!/bin/sh\n\
                 case \"$*\" in\n\
                 *attach-session*) stty raw -echo; : > {ready}; exec cat >> {out} ;;\n\
                 *'#{{pane_dead_status}}'*) printf '1 0\\n'; exit 0 ;;\n\
                 *display-message*) printf '1234 0 0 80 24 0\\n'; exit 0 ;;\n\
                 esac\n\
                 exit 0\n",
                ready = ready.display(),
                out = out.display()
            ),
        )
        .unwrap();
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
        (script.to_string_lossy().into_owned(), out, ready)
    }

    async fn wait_until(what: &str, mut ok: impl FnMut() -> bool) {
        let start = std::time::Instant::now();
        while !ok() {
            assert!(
                start.elapsed() < Duration::from_secs(10),
                "timed out waiting for {what}"
            );
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    }

    /// `viewer_open`, `take_size`, `keys`, `viewer_close` and `kill` through
    /// the real client against the real runner server (a fake tmux and a
    /// recording supervisor stand in for the host): each returns its real
    /// payload and has its real effect. Before this, only the no-session
    /// error path of three of them was exercised.
    #[tokio::test]
    async fn id_carrying_ops_succeed_through_the_real_client_and_server() {
        use crate::runner::config::RunnerConfig;
        use crate::runner::server::{bind_runner_socket, serve};
        use crate::runner::supervisor::fake::FakeSupervisor;
        use crate::runner::Runner;

        let dir = tempfile::tempdir().unwrap();
        let (tmux, out, ready) = fake_tmux(dir.path());
        let cfg =
            RunnerConfig::test_config(dir.path().join("run"), tmux, PathBuf::from("/dev/null"));
        let runner = Arc::new(Runner::with_supervisor(cfg, FakeSupervisor::new()));
        let sessions = runner.sessions_dir();
        std::fs::create_dir_all(&sessions).unwrap();
        // Discovery globs `s/<id>.sock`: a placeholder is a live holder to
        // the fake tmux.
        let holder = sessions.join("abcd1234.sock");
        std::fs::write(&holder, b"").unwrap();
        let sock = runner.cfg.run_dir.join("runner").join("runner.sock");
        std::fs::create_dir_all(sock.parent().unwrap()).unwrap();
        let listener = bind_runner_socket(&sock).unwrap();
        let (tx, rx) = tokio::sync::broadcast::channel::<()>(1);
        let server = tokio::spawn({
            let runner = runner.clone();
            let sock = sock.clone();
            async move {
                let _ = serve(runner, listener, sock, rx).await;
            }
        });

        let mut client = RunnerClient::connect(&sock).await.unwrap();
        let id = "abcd1234".to_string();

        let opened = client
            .call(&Request::ViewerOpen {
                id: id.clone(),
                viewer: "v1".to_string(),
            })
            .await
            .unwrap();
        assert_eq!(opened, json!({"viewer": "v1"}));
        assert!(runner.has_viewer(&id, "v1").await, "the viewer registered");

        // Typed bytes reach the hidden client once its pty is raw.
        wait_until("the hidden client", || ready.exists()).await;
        let keys = client
            .call(&Request::Keys {
                id: id.clone(),
                viewer: "v1".to_string(),
                input: crate::wire::KeyInput::Named {
                    named: vec!["Up".to_string(), "y".to_string()],
                },
            })
            .await
            .unwrap();
        assert_eq!(keys, json!({}));
        wait_until("the typed bytes", || {
            std::fs::read(&out).is_ok_and(|b| b.len() >= 4)
        })
        .await;
        assert_eq!(std::fs::read(&out).unwrap(), b"\x1b[Ay");

        let taken = client
            .call(&Request::TakeSize {
                id: id.clone(),
                cols: 90,
                rows: 30,
                owner: "attach-test".to_string(),
                force: false,
            })
            .await
            .unwrap();
        assert_eq!(
            taken,
            json!({"cols": 90, "rows": 30, "owner": "attach-test"})
        );

        let closed = client
            .call(&Request::ViewerClose {
                id: id.clone(),
                viewer: "v1".to_string(),
            })
            .await
            .unwrap();
        assert_eq!(closed, json!({"viewer": "v1"}));
        assert!(!runner.has_viewer(&id, "v1").await, "the viewer is gone");
        // The same viewer cannot be closed twice.
        let again = client
            .call(&Request::ViewerClose {
                id: id.clone(),
                viewer: "v1".to_string(),
            })
            .await
            .unwrap_err();
        assert_eq!(again.code(), Some("no-viewer"));

        let killed = client
            .call(&Request::Kill {
                id: id.clone(),
                grace_secs: Some(2),
            })
            .await
            .unwrap();
        assert_eq!(killed, json!({"exit_code": 0, "forced": false}));
        assert!(!holder.exists(), "kill removed the holder socket");
        assert!(
            runner
                .supervisor
                .calls
                .lock()
                .unwrap()
                .contains(&("stop".to_string(), id.clone())),
            "kill stopped the session through the supervisor"
        );
        // One connection carried the whole conversation, still paired.
        assert!(client.call(&Request::Ping {}).await.is_ok());
        let _ = tx.send(());
        let _ = tokio::time::timeout(Duration::from_secs(10), server).await;
    }
}
