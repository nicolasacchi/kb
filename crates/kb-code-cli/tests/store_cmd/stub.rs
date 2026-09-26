//! A one-shot HTTP stub standing in for `kb-code-server`.
//!
//! `store_cmd::run` decides its exit code from the daemon's JSON body, and
//! the decision is inline in the arm (there is no pure helper to call), so
//! the only way to pin it is to hand the real `kb-code` binary a daemon
//! answer of our choosing. This serves exactly that: HTTP 200 + a JSON
//! body, for at most `max` requests, then it stops.
//!
//! The one-shot shape is deliberate — each `kb-code store …` invocation
//! makes exactly ONE round trip, so a stub that served more would be
//! accepting connections nobody can account for.

use std::io::{ErrorKind, Read, Write};
use std::net::{Shutdown, TcpListener, TcpStream};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

pub struct StubDaemon {
    pub url: String,
    requests: Arc<AtomicUsize>,
    handle: Option<std::thread::JoinHandle<()>>,
}

impl StubDaemon {
    /// Answer up to `max` requests with `body` (HTTP 200, `application/json`)
    /// while `window` is open, then stop listening.
    ///
    /// `max = 0` is the "the CLI must not talk to the daemon at all" probe:
    /// the listener stays bound for the whole `window`, so a stray request
    /// is REFUSED (and would show up as a transport error, not a 200).
    pub fn json(body: &str, max: usize, window: Duration) -> StubDaemon {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind stub");
        let addr = listener.local_addr().expect("stub addr");
        listener
            .set_nonblocking(true)
            .expect("stub listener nonblocking");
        let body = body.to_string();
        let requests = Arc::new(AtomicUsize::new(0));
        let counted = Arc::clone(&requests);
        let handle = std::thread::spawn(move || {
            let deadline = Instant::now() + window;
            // `max == 0` polls the whole window and never accepts, so a
            // request the CLI was NOT supposed to make cannot be served
            // into a passing test.
            while Instant::now() < deadline && (max == 0 || counted.load(Ordering::SeqCst) < max) {
                match listener.accept() {
                    Ok((sock, _)) => {
                        counted.fetch_add(1, Ordering::SeqCst);
                        serve(sock, &body);
                    }
                    Err(e) if e.kind() == ErrorKind::WouldBlock => {
                        std::thread::sleep(Duration::from_millis(5));
                    }
                    Err(_) => break,
                }
            }
        });
        StubDaemon {
            url: format!("http://{addr}"),
            requests,
            handle: Some(handle),
        }
    }

    /// How many requests the CLI actually made.
    pub fn request_count(&self) -> usize {
        self.requests.load(Ordering::SeqCst)
    }

    /// Wait for the stub's window to close and stop it.
    pub fn join(mut self) -> usize {
        if let Some(h) = self.handle.take() {
            let _ = h.join();
        }
        self.request_count()
    }
}

/// Read one HTTP request (headers + `Content-Length` body) and answer it.
fn serve(mut sock: TcpStream, body: &str) {
    let _ = sock.set_read_timeout(Some(Duration::from_secs(10)));
    let mut buf: Vec<u8> = Vec::new();
    let mut chunk = [0u8; 4096];
    loop {
        match sock.read(&mut chunk) {
            Ok(0) | Err(_) => break,
            Ok(n) => buf.extend_from_slice(&chunk[..n]),
        }
        if let Some(p) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
            let head = String::from_utf8_lossy(&buf[..p]).to_ascii_lowercase();
            let len: usize = head
                .lines()
                .find_map(|l| l.strip_prefix("content-length:")?.trim().parse().ok())
                .unwrap_or(0);
            if buf.len() >= p + 4 + len {
                break;
            }
        }
    }
    let len = body.len();
    let resp = format!(
        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {len}\r\n\
         Connection: close\r\n\r\n{body}"
    );
    let _ = sock.write_all(resp.as_bytes());
    let _ = sock.flush();
    let _ = sock.shutdown(Shutdown::Write);
}
