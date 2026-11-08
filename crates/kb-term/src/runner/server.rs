//! `runner.sock`: newline-delimited JSON over a unix stream.
//!
//! Many requests per connection; the request line is capped at 64 KiB
//! (longer -> `bad-request`, connection closed). Responses answer in the
//! envelope `{v: 1, id, status: "ok"|"err", ...}`.
//!
//! The socket is bound under a 077 umask and `chmod 0600`. An existing
//! path that answers a probe connection means "another runner is
//! running"; a dead socket file is unlinked and re-bound. A connection
//! whose peer uid differs from the runner's uid is rejected.
//!
//! On SIGTERM/SIGINT kb-termd stops accepting, closes anchor masters,
//! removes `runner.sock` and leaves every holder alone.
use std::path::{Path, PathBuf};
use std::sync::Arc;

use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::{UnixListener, UnixStream};
use tokio::sync::broadcast;

use serde_json::{json, Value};

use crate::runner::rpc;
use crate::runner::supervisor::Supervisor;
use crate::runner::Runner;

/// The default umask for the runner socket (then `chmod 0600` as well).
const RUNNER_UMASK: u32 = 0o077;
/// 64 KiB request line cap.
const MAX_LINE: usize = 64 * 1024;

/// Bind `runner.sock`, replacing a stale socket file.
pub fn bind_runner_socket(path: &Path) -> std::io::Result<UnixListener> {
    if path.exists() {
        if probe_live(path) {
            return Err(std::io::Error::new(
                std::io::ErrorKind::AddrInUse,
                "another runner is running",
            ));
        }
        let _ = std::fs::remove_file(path);
    }
    let old_umask = unsafe { libc::umask(RUNNER_UMASK) };
    let result = UnixListener::bind(path);
    unsafe {
        libc::umask(old_umask);
    }
    let listener = result?;
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
    Ok(listener)
}

fn probe_live(path: &Path) -> bool {
    // A live runner answers a connect; a dead socket file fails with
    // ECONNREFUSED. Any successful connect means "someone is listening".
    std::os::unix::net::UnixStream::connect(path).is_ok()
}

/// The peer uid of a connected stream (`SO_PEERCRED`).
fn peer_uid(stream: &UnixStream) -> Option<u32> {
    use std::os::unix::io::AsRawFd;
    let mut cred: libc::ucred = unsafe { std::mem::zeroed() };
    let mut len = std::mem::size_of::<libc::ucred>() as libc::socklen_t;
    let rc = unsafe {
        libc::getsockopt(
            stream.as_raw_fd(),
            libc::SOL_SOCKET,
            libc::SO_PEERCRED,
            &mut cred as *mut _ as *mut libc::c_void,
            &mut len,
        )
    };
    if rc == 0 {
        Some(cred.uid)
    } else {
        None
    }
}

/// Serve until a shutdown signal arrives. `shutdown` is signalled by the
/// caller (SIGTERM/SIGINT handling lives in the binary); the socket file
/// is removed and anchors closed before returning. Holders are untouched.
pub async fn serve<S: Supervisor + 'static>(
    runner: Arc<Runner<S>>,
    listener: UnixListener,
    sock_path: PathBuf,
    shutdown: broadcast::Receiver<()>,
) -> std::io::Result<()> {
    let mut shutdown = shutdown;
    loop {
        tokio::select! {
            _ = shutdown.recv() => {
                // Stop accepting, close anchor masters, remove the socket,
                // leave every holder alone.
                runner.close_all_anchors().await;
                runner.close_viewers_for_all().await;
                drop(listener);
                let _ = std::fs::remove_file(&sock_path);
                return Ok(());
            }
            accepted = listener.accept() => {
                match accepted {
                    Ok((stream, _)) => {
                        if peer_uid(&stream) != Some(runner.server_uid) {
                            // Wrong-uid connection: reject silently.
                            continue;
                        }
                        tokio::spawn(handle_connection(runner.clone(), stream));
                    }
                    Err(e) => {
                        tracing::warn!(error = %e, "accept failed");
                    }
                }
            }
        }
    }
}

async fn handle_connection<S: Supervisor + 'static>(runner: Arc<Runner<S>>, stream: UnixStream) {
    let (read_half, mut write_half) = stream.into_split();
    let mut reader = BufReader::new(read_half);
    loop {
        let mut line = String::new();
        // Bounded read: an unterminated line can never buffer past
        // MAX_LINE + 1 bytes, so an oversized client is answered
        // `bad-request` instead of exhausting memory.
        let n = match (&mut reader)
            .take((MAX_LINE + 1) as u64)
            .read_line(&mut line)
            .await
        {
            Ok(n) => n,
            Err(_) => return,
        };
        if n == 0 {
            return; // EOF
        }
        if n > MAX_LINE && !line.ends_with('\n') {
            // Over-long request: bad-request and close the connection.
            let err = crate::wire::Response::Err {
                code: "bad-request".to_string(),
                message: "request line exceeds 64 KiB".to_string(),
            };
            let _ = respond(&mut write_half, None, err).await;
            return;
        }
        let req_id = request_id(&line);
        // The `stream` op splices onto this connection: it is the LAST
        // request this connection ever answers. A successful open is
        // answered with the standard ok envelope, then the connection
        // becomes a tap relay (see `crate::runner::stream`). An open
        // failure is an ordinary error reply and the connection closes.
        if let Ok(crate::wire::Request::Stream { id, from }) = rpc::parse_request(line.as_bytes()) {
            if !crate::runner::ids::valid_id(&id) {
                let err = crate::wire::Response::Err {
                    code: "invalid-id".to_string(),
                    message: format!("invalid terminal id `{id}`"),
                };
                let _ = respond(&mut write_half, req_id, err).await;
                return;
            }
            // The client-side halves for the splice: the read half
            // comes from the line reader (any spliced frame starts
            // here), the write half is the one the RPC loop holds.
            let client_read = reader.into_inner();
            let tap = match crate::runner::stream::connect_tap(&runner, &id, from).await {
                Ok(tap) => tap,
                Err(e) => {
                    let err = crate::wire::Response::Err {
                        code: e.code().to_string(),
                        message: e.message(),
                    };
                    let _ = respond(&mut write_half, req_id, err).await;
                    return;
                }
            };
            // The ok envelope goes out first so the client starts
            // decoding envelopes only after the handshake.
            let ok = crate::wire::Response::Ok { data: json!({}) };
            if respond(&mut write_half, req_id, ok).await.is_err() {
                return;
            }
            let (tap_read, tap_write) = tap.into_split();
            let _ =
                crate::runner::stream::splice(tap_read, tap_write, client_read, write_half).await;
            return;
        }
        let resp = match rpc::parse_request(line.as_bytes()) {
            Ok(req) => rpc::dispatch(&runner, &req).await,
            Err(e) => crate::wire::Response::Err {
                code: e.code().to_string(),
                message: e.message(),
            },
        };
        if respond(&mut write_half, req_id, resp).await.is_err() {
            return;
        }
    }
}

fn request_id(line: &str) -> Option<Value> {
    serde_json::from_str::<Value>(line)
        .ok()
        .and_then(|v| v.get("id").cloned())
}

async fn respond(
    w: &mut tokio::net::unix::OwnedWriteHalf,
    id: Option<Value>,
    resp: crate::wire::Response,
) -> std::io::Result<()> {
    let mut out = serde_json::to_value(&resp).unwrap_or_else(|_| json!({"status": "err"}));
    if let Some(obj) = out.as_object_mut() {
        obj.insert("v".to_string(), json!(1));
        obj.insert("id".to_string(), id.unwrap_or(Value::Null));
    }
    let mut line = serde_json::to_string(&out).unwrap_or_default();
    line.push('\n');
    w.write_all(line.as_bytes()).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::runner::config::RunnerConfig;
    use crate::runner::supervisor::DirectSupervisor;
    use crate::runner::Runner;
    use std::path::PathBuf;

    /// Spawn the serve loop with a stub handler path exercised through the
    /// real dispatch (ping needs no tmux) inside `dir`.
    async fn stub_runner(
        dir: &Path,
    ) -> (
        tokio::task::JoinHandle<()>,
        Arc<Runner<DirectSupervisor>>,
        broadcast::Sender<()>,
    ) {
        let cfg = RunnerConfig::test_config(
            dir.to_path_buf(),
            "/bin/true".to_string(),
            PathBuf::from("/dev/null"),
        );
        let runner = Arc::new(Runner::direct(cfg));
        let sock = runner.cfg.run_dir.join("runner").join("runner.sock");
        let _ = std::fs::create_dir_all(sock.parent().unwrap());
        let listener = bind_runner_socket(&sock).unwrap();
        let (tx, rx) = broadcast::channel::<()>(1);
        let server_runner = runner.clone();
        let handle = tokio::spawn(async move {
            let _ = serve(server_runner, listener, sock, rx).await;
        });
        (handle, runner, tx)
    }

    async fn rpc_roundtrip(sock: &Path, request: &str) -> String {
        let mut stream = UnixStream::connect(sock).await.unwrap();
        stream.write_all(request.as_bytes()).await.unwrap();
        stream.write_all(b"\n").await.unwrap();
        let mut reader = BufReader::new(stream);
        let mut line = String::new();
        tokio::time::timeout(
            std::time::Duration::from_secs(10),
            reader.read_line(&mut line),
        )
        .await
        .expect("rpc roundtrip must answer within 10 s")
        .unwrap();
        line
    }

    #[tokio::test]
    async fn runner_sock_is_0600_and_second_runner_refuses() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().to_path_buf();
        let (handle, runner, _tx) = stub_runner(&root).await;
        let sock = runner.cfg.run_dir.join("runner").join("runner.sock");
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(&sock).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600, "runner.sock must be 0600");
        let resp = rpc_roundtrip(&sock, r#"{"v":1,"id":7,"op":"ping"}"#).await;
        assert!(resp.contains(r#""v":1"#), "envelope version: {resp}");
        assert!(resp.contains(r#""id":7"#), "envelope id echoes: {resp}");
        assert!(resp.contains(r#""status":"ok""#), "ping ok: {resp}");
        // A second runner on the same socket must refuse.
        let second = bind_runner_socket(&sock);
        assert!(second.is_err(), "second runner must refuse to bind");
        assert_eq!(second.unwrap_err().to_string(), "another runner is running");
        handle.abort();
        let _ = std::fs::remove_file(&sock);
    }

    #[tokio::test]
    async fn stale_socket_file_is_replaced() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().to_path_buf();
        let sock = root.join("runner").join("runner.sock");
        let _ = std::fs::create_dir_all(sock.parent().unwrap());
        // Bind then drop the listener WITHOUT unlinking: a dead socket file.
        drop(bind_runner_socket(&sock).unwrap());
        assert!(sock.exists(), "dead socket file must exist");
        let listener = bind_runner_socket(&sock).expect("stale socket must be replaced");
        drop(listener);
        let _ = std::fs::remove_file(&sock);
    }

    #[tokio::test]
    async fn overlong_line_gets_bad_request() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().to_path_buf();
        let (handle, runner, _tx) = stub_runner(&root).await;
        let sock = runner.cfg.run_dir.join("runner").join("runner.sock");
        // 128 KiB without a newline: the server must answer bad-request
        // from a bounded read (never buffer the whole line).
        let mut stream = UnixStream::connect(&sock).await.unwrap();
        let big = vec![b'x'; 128 * 1024];
        let _ = stream.write_all(&big).await; // server closes early; EPIPE ok
        let mut reader = BufReader::new(stream);
        let mut line = String::new();
        tokio::time::timeout(
            std::time::Duration::from_secs(10),
            reader.read_line(&mut line),
        )
        .await
        .expect("overlong line must be answered within 10 s")
        .unwrap();
        assert!(
            line.contains("bad-request"),
            "overlong line must be bad-request: {line}"
        );
        handle.abort();
        let _ = std::fs::remove_file(&sock);
    }
}
