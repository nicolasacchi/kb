//! The operator-local runner client: newline-delimited JSON over
//! `runner.sock` (`runner/server.rs` framing, `{v:1, id, op, ...}` /
//! `{v:1, id, status: "ok"|"err", ...}`), backed by the wire types in
//! [`crate::wire`].
//!
//! Resolution order for the runner socket:
//! `--runner-sock` (flag) > `$KB_TERM_RUNNER_SOCK` > `$KB_TERM_RUN_DIR`
//! + `/runner/runner.sock` > `/run/kb-term/runner/runner.sock`.
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
    if let Some(p) = flag {
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
        let mut line = request_line(id, req).map_err(RunnerClientError::Unreachable)?;
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
        // The envelope id must echo our request id (a late or duplicate
        // line is a protocol failure, never a silent mis-pairing).
        match serde_json::from_str::<serde_json::Value>(&body)
            .ok()
            .and_then(|v| v.get("id").and_then(serde_json::Value::as_u64))
        {
            Some(r) if r == id => {}
            Some(_) => {
                return Err(RunnerClientError::Unreachable(
                    "runner socket answered an unexpected id".to_string(),
                ));
            }
            None => {
                return Err(RunnerClientError::Unreachable(
                    "runner reply has no id".to_string(),
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

/// Build the request line `{"v":1,"id":n,"op":...}` for a wire request.
fn request_line(id: u64, req: &Request) -> Result<String, String> {
    let mut v = serde_json::to_value(req).map_err(|e| format!("encode request: {e}"))?;
    let obj = v
        .as_object_mut()
        .ok_or_else(|| "request is not an object".to_string())?;
    obj.insert("v".to_string(), serde_json::json!(1));
    obj.insert("id".to_string(), serde_json::json!(id));
    serde_json::to_string(&v).map_err(|e| format!("encode request: {e}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::wire::Harness;

    #[test]
    fn runner_socket_resolution_order() {
        // The env fallback order is verified against the live env:
        // set/unset each and check the precedence.
        std::env::remove_var(RUNNER_SOCK_ENV);
        std::env::remove_var(RUN_DIR_ENV);
        assert_eq!(
            resolve_runner_socket(None),
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
        std::env::remove_var(RUNNER_SOCK_ENV);
        std::env::remove_var(RUN_DIR_ENV);
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
        let line = request_line(7, &Request::Ping {}).unwrap();
        let v: serde_json::Value = serde_json::from_str(&line).unwrap();
        assert_eq!(v["v"], 1);
        assert_eq!(v["id"], 7);
        assert_eq!(v["op"], "ping");
        let line = request_line(
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
}
