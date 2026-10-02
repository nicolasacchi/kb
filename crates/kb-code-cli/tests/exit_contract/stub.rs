//! A tiny HTTP stub answering every request with a chosen status and body.

use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::time::Duration;

pub struct StatusStub {
    pub url: String,
}

impl StatusStub {
    pub fn new(status: u16, body: &str) -> StatusStub {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind stub");
        let url = format!("http://{}", listener.local_addr().unwrap());
        let body = body.to_string();
        std::thread::spawn(move || {
            // Serve until the test process exits.
            for sock in listener.incoming().flatten() {
                serve(sock, status, &body);
            }
        });
        StatusStub { url }
    }
}

fn serve(mut sock: TcpStream, status: u16, body: &str) {
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
    let resp = format!(
        "HTTP/1.1 {status} X\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\
         Connection: close\r\n\r\n{body}",
        body.len()
    );
    let _ = sock.write_all(resp.as_bytes());
    let _ = sock.flush();
    let _ = sock.shutdown(std::net::Shutdown::Write);
}
