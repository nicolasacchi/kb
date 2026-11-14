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
pub struct RunnerClient {
    stream: tokio::net::UnixStream,
    next_id: u64,
    /// The socket path (for error messages).
    path: PathBuf,
    /// Per-call deadline.
    timeout: Duration,
}

impl RunnerClient {
    /// Connect to `path`.
    pub async fn connect(path: &Path) -> Result<Self, RunnerClientError> {
        let stream = tokio::net::UnixStream::connect(path).await.map_err(|e| {
            RunnerClientError::Unreachable(format!(
                "cannot connect to runner socket {}: {e}",
                path.display()
            ))
        })?;
        Ok(Self {
            stream,
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
        self.stream.write_all(line.as_bytes()).await.map_err(|_| {
            RunnerClientError::Unreachable(format!(
                "write to runner socket {} failed",
                self.path.display()
            ))
        })?;
        let body = tokio::time::timeout(self.timeout, read_response_line(&mut self.stream))
            .await
            .map_err(|_| {
                RunnerClientError::Unreachable(format!(
                    "runner socket {} did not answer in time",
                    self.path.display()
                ))
            })?
            .map_err(|_| {
                RunnerClientError::Unreachable(format!(
                    "cannot read runner socket {}",
                    self.path.display()
                ))
            })?;
        // The reply must echo the envelope id we sent: the numeric request
        // id, or for an id-carrying op the op's own session id (the server
        // echoes whatever `id` the line carried). A late or duplicate line
        // is a protocol failure, never a silent mis-pairing.
        match serde_json::from_str::<serde_json::Value>(&body)
            .ok()
            .and_then(|v| v.get("id").cloned())
        {
            Some(r) if r == expected => {}
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

/// Read one response line, bounded by the server's 64 KiB line cap.
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
}
