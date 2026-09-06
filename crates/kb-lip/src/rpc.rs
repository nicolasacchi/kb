//! JSON-RPC 2.0 stdio framing: `Content-Length: N\r\n\r\n<json bytes>`
//! (the LSP wire format — LSP itself is "JSON-RPC 2.0 over
//! Content-Length-framed stdio"). Two flavors sharing the same wire
//! format:
//!
//! - [`blocking`] — plain `std::io`, used by the test-only fake LSP
//!   fixture (`src/bin/fake_lsp.rs`), which is its own OS process and has
//!   no reason to pull in tokio.
//! - [`framed`] — `tokio::io`, used by the real client (`lsp.rs`) talking
//!   to the child language server's piped stdio from inside the async
//!   adapter.
//!
//! Both are pinned against the SAME `framing_round_trips` golden shape
//! test below so the two implementations can never silently diverge on
//! the wire format.

pub mod blocking {
    use serde_json::Value;
    use std::io::{self, BufRead, Write};

    pub fn write_message<W: Write>(w: &mut W, value: &Value) -> io::Result<()> {
        let body = serde_json::to_vec(value)?;
        write!(w, "Content-Length: {}\r\n\r\n", body.len())?;
        w.write_all(&body)?;
        w.flush()
    }

    /// Reads one framed message. `Ok(None)` means clean EOF before any
    /// header bytes arrived (the peer closed its write end) — callers
    /// treat this as "the process is done talking", not a parse error.
    pub fn read_message<R: BufRead>(r: &mut R) -> io::Result<Option<Value>> {
        let mut content_length: Option<usize> = None;
        loop {
            let mut line = String::new();
            let n = r.read_line(&mut line)?;
            if n == 0 {
                return Ok(None);
            }
            let line = line.trim_end_matches(['\r', '\n']);
            if line.is_empty() {
                break;
            }
            if let Some(v) = line.strip_prefix("Content-Length:") {
                content_length = Some(v.trim().parse().map_err(|_| {
                    io::Error::new(io::ErrorKind::InvalidData, "bad Content-Length header")
                })?);
            }
            // Any other header (e.g. Content-Type) is ignored per the LSP spec.
        }
        let len = content_length.ok_or_else(|| {
            io::Error::new(io::ErrorKind::InvalidData, "missing Content-Length header")
        })?;
        let mut buf = vec![0u8; len];
        r.read_exact(&mut buf)?;
        let value: Value = serde_json::from_slice(&buf)?;
        Ok(Some(value))
    }
}

pub mod framed {
    use serde_json::Value;
    use std::io;
    use tokio::io::{AsyncBufRead, AsyncBufReadExt, AsyncReadExt, AsyncWrite, AsyncWriteExt};

    pub async fn write_message<W: AsyncWrite + Unpin>(w: &mut W, value: &Value) -> io::Result<()> {
        let body = serde_json::to_vec(value)?;
        let header = format!("Content-Length: {}\r\n\r\n", body.len());
        w.write_all(header.as_bytes()).await?;
        w.write_all(&body).await?;
        w.flush().await
    }

    /// Async twin of [`super::blocking::read_message`] — same framing,
    /// same EOF-before-any-bytes convention (`Ok(None)`).
    pub async fn read_message<R: AsyncBufRead + Unpin>(r: &mut R) -> io::Result<Option<Value>> {
        let mut content_length: Option<usize> = None;
        loop {
            let mut line = String::new();
            let n = r.read_line(&mut line).await?;
            if n == 0 {
                return Ok(None);
            }
            let line = line.trim_end_matches(['\r', '\n']);
            if line.is_empty() {
                break;
            }
            if let Some(v) = line.strip_prefix("Content-Length:") {
                content_length = Some(v.trim().parse().map_err(|_| {
                    io::Error::new(io::ErrorKind::InvalidData, "bad Content-Length header")
                })?);
            }
        }
        let len = content_length.ok_or_else(|| {
            io::Error::new(io::ErrorKind::InvalidData, "missing Content-Length header")
        })?;
        let mut buf = vec![0u8; len];
        r.read_exact(&mut buf).await?;
        let value: Value = serde_json::from_slice(&buf)?;
        Ok(Some(value))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn blocking_write_produces_the_exact_lsp_wire_shape() {
        let value = json!({"jsonrpc": "2.0", "id": 1, "method": "initialize"});
        let mut buf = Vec::new();
        blocking::write_message(&mut buf, &value).unwrap();
        let body = serde_json::to_vec(&value).unwrap();
        let expected = format!(
            "Content-Length: {}\r\n\r\n{}",
            body.len(),
            String::from_utf8(body).unwrap()
        );
        assert_eq!(String::from_utf8(buf).unwrap(), expected);
    }

    #[test]
    fn blocking_round_trips_one_message() {
        let value = json!({"jsonrpc": "2.0", "id": 7, "result": {"ok": true}});
        let mut buf = Vec::new();
        blocking::write_message(&mut buf, &value).unwrap();
        let mut cursor = std::io::Cursor::new(buf);
        let got = blocking::read_message(&mut cursor).unwrap().unwrap();
        assert_eq!(got, value);
    }

    #[test]
    fn blocking_round_trips_two_back_to_back_messages() {
        let a = json!({"jsonrpc": "2.0", "id": 1, "method": "a"});
        let b = json!({"jsonrpc": "2.0", "id": 2, "method": "b"});
        let mut buf = Vec::new();
        blocking::write_message(&mut buf, &a).unwrap();
        blocking::write_message(&mut buf, &b).unwrap();
        let mut cursor = std::io::Cursor::new(buf);
        assert_eq!(blocking::read_message(&mut cursor).unwrap().unwrap(), a);
        assert_eq!(blocking::read_message(&mut cursor).unwrap().unwrap(), b);
        assert_eq!(blocking::read_message(&mut cursor).unwrap(), None);
    }

    #[test]
    fn blocking_clean_eof_before_any_bytes_is_none() {
        let mut cursor = std::io::Cursor::new(Vec::<u8>::new());
        assert_eq!(blocking::read_message(&mut cursor).unwrap(), None);
    }

    #[test]
    fn blocking_missing_content_length_header_errors() {
        let mut cursor = std::io::Cursor::new(b"Content-Type: foo\r\n\r\n{}".to_vec());
        let err = blocking::read_message(&mut cursor).unwrap_err();
        assert_eq!(err.kind(), std::io::ErrorKind::InvalidData);
    }

    #[test]
    fn blocking_ignores_extra_headers() {
        let value = json!({"jsonrpc": "2.0", "id": 1});
        let body = serde_json::to_vec(&value).unwrap();
        let raw = format!(
            "Content-Type: application/vscode-jsonrpc; charset=utf-8\r\nContent-Length: {}\r\n\r\n{}",
            body.len(),
            String::from_utf8(body).unwrap()
        );
        let mut cursor = std::io::Cursor::new(raw.into_bytes());
        let got = blocking::read_message(&mut cursor).unwrap().unwrap();
        assert_eq!(got, value);
    }

    #[tokio::test]
    async fn framed_round_trips_one_message() {
        let value = json!({"jsonrpc": "2.0", "id": 7, "result": {"ok": true}});
        let mut buf = Vec::new();
        framed::write_message(&mut buf, &value).await.unwrap();
        let mut reader = tokio::io::BufReader::new(std::io::Cursor::new(buf));
        let got = framed::read_message(&mut reader).await.unwrap().unwrap();
        assert_eq!(got, value);
    }

    #[tokio::test]
    async fn framed_and_blocking_write_produce_byte_identical_output() {
        let value = json!({"jsonrpc": "2.0", "id": 1, "method": "textDocument/hover"});
        let mut blocking_buf = Vec::new();
        blocking::write_message(&mut blocking_buf, &value).unwrap();
        let mut framed_buf = Vec::new();
        framed::write_message(&mut framed_buf, &value)
            .await
            .unwrap();
        assert_eq!(blocking_buf, framed_buf);
    }
}
