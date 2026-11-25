//! `GET /api/terms/{id}/ls?path=&root=` and `GET /api/terms/{id}/file?path=&root=`
//! (FXLS): list and download files from a terminal's working area.
//!
//! The runner owns the files (kb-server runs in a container that cannot
//! see the host's working directories) and enforces containment in the
//! kernel (`openat2`, see `kb-term`'s `runner::files`). This module only
//! relays: it validates the request shape, asks the runner, and shapes the
//! HTTP answer. It reads no file itself and holds no path it was not handed
//! by the runner.
//!
//! **The gate is the shared GET gate** (`gate::gate_read`), the FIRST
//! statement of both handlers: tier enabled (else a bodiless 404), the
//! `X-Kb-Intent: terminal` header, an exact-if-present `Origin`,
//! `Sec-Fetch-Site` of `same-origin`/`none` when present, no token callers,
//! and no loopback bypass. A refused request never dials the runner.
//!
//! **A download is never rendered.** The answer is always
//! `Content-Type: application/octet-stream`, `Content-Disposition:
//! attachment` with a sanitised file name, `X-Content-Type-Options:
//! nosniff` and `Cache-Control: no-store`, whatever the file is or is
//! called: a downloaded `.html` cannot run on the kb origin, and no type is
//! sniffed or taken from the sender. Raw bytes cannot be scrubbed, so the
//! route lives strictly inside the terminal tier and persists nothing.
//!
//! **Streamed, never buffered whole.** The body is pulled from the runner
//! one chunk of at most [`CHUNK_MAX_BYTES`] at a time (`fs_get` with a
//! running offset), the next pull starting only when the previous chunk was
//! written, so peak memory is one chunk however large the file. The
//! per-file cap is [`FILE_MAX_BYTES`] (32 MiB, the same constant as the
//! upload door): a larger file is `413` before any byte. If a later chunk
//! fails, or the file's size or mtime changed since the first chunk, the
//! stream ends with an error, so the client sees a body shorter than the
//! `Content-Length` it was promised rather than a spliced file.
//!
//! The browser cannot send the intent header on a plain `<a download>`
//! navigation, so the SPA fetches with the header, takes a blob and saves
//! it through an object-URL anchor; `ls` is the same fetch returning JSON.
//! Nothing here emits an SSE event.

use std::sync::Arc;

use axum::body::{Body, Bytes};
use axum::extract::rejection::QueryRejection;
use axum::extract::{Path, Query, State};
use axum::http::{header, HeaderMap, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::Extension;
use serde::Deserialize;

use crate::middleware::Identity;
use crate::routes::terms::{
    bad_request, no_store, payload_too_large, runner_bad_reply, runner_down, runner_err, runner_for,
};
use crate::state::KbHandles;
use crate::terminal::binding::is_valid_term_id;
use crate::terminal::gate::gate_read;
use crate::terminal::runner_client::{FsGetReply, RunnerError, UnixRunner};
use crate::terminal::upload::{CHUNK_MAX_BYTES, FILE_MAX_BYTES};
use crate::terminal::wire::{RunnerReply, RunnerRequest, TermLsAnswer};

/// `urn:kb:errors:terminal-not-a-file` — the path names something that is
/// not a regular file (or, for a listing, not a directory): a symlink,
/// fifo, socket or device.
pub(crate) const ERR_TERMINAL_NOT_A_FILE: &str = "urn:kb:errors:terminal-not-a-file";
/// `urn:kb:errors:terminal-outside-root` — the path names a place outside
/// the terminal's working area.
pub(crate) const ERR_TERMINAL_OUTSIDE_ROOT: &str = "urn:kb:errors:terminal-outside-root";

/// Longest `path` accepted, in bytes (the Linux `PATH_MAX`, as in the runner).
pub(crate) const PATH_MAX_BYTES: usize = 4096;

/// Longest download file name carried in `Content-Disposition`, in bytes.
pub(crate) const DOWNLOAD_NAME_MAX_BYTES: usize = 120;

#[derive(Debug, Deserialize)]
pub struct PathQuery {
    path: Option<String>,
    root: Option<String>,
}

/// Validate the request-shaped parts shared by both routes. Nothing here
/// touches the runner: a bad value is a 400 with the runner never dialled.
fn parse_query(
    query: Result<Query<PathQuery>, QueryRejection>,
    require_path: bool,
) -> Result<(String, String), Response<Body>> {
    let Ok(Query(q)) = query else {
        return Err(bad_request("invalid query"));
    };
    let root = match q.root.as_deref() {
        None | Some("cwd") => "cwd",
        Some("up") => "up",
        Some(_) => return Err(bad_request("root must be `cwd` or `up`")),
    };
    let path = q.path.unwrap_or_default();
    if require_path && path.is_empty() {
        return Err(bad_request("path is required"));
    }
    if path.len() > PATH_MAX_BYTES {
        return Err(bad_request("path is longer than 4096 bytes"));
    }
    if path.contains('\0') {
        return Err(bad_request("path contains a NUL byte"));
    }
    Ok((root.to_string(), path))
}

/// Map a failed runner call to its response (never carries the socket path).
fn failure(e: RunnerError) -> Response<Body> {
    match e {
        RunnerError::Unavailable | RunnerError::Timeout => runner_down(),
        RunnerError::TooLarge | RunnerError::BadReply => runner_bad_reply(),
    }
}

/// `GET /api/terms/{id}/ls`.
pub async fn ls(
    State(state): State<Arc<KbHandles>>,
    Extension(identity): Extension<Identity>,
    headers: HeaderMap,
    Path(id): Path<String>,
    query: Result<Query<PathQuery>, QueryRejection>,
) -> Response<Body> {
    if let Err(e) = gate_read(&state, &headers, &identity).await {
        return e;
    }
    if !is_valid_term_id(&id) {
        return bad_request("invalid terminal id");
    }
    let (root, path) = match parse_query(query, false) {
        Ok(v) => v,
        Err(e) => return e,
    };
    let runner = runner_for(&state).await;
    match runner.call(&RunnerRequest::FsLs { id, root, path }).await {
        Err(e) => failure(e),
        Ok(RunnerReply::Err { code, message }) => runner_err(&code, &message),
        Ok(RunnerReply::Ok { data }) => match serde_json::from_value::<TermLsAnswer>(data) {
            Ok(answer) => no_store(axum::Json(answer).into_response()),
            Err(_) => runner_bad_reply(),
        },
    }
}

/// The state of one streamed download between chunk pulls.
struct Pull {
    runner: UnixRunner,
    id: String,
    root: String,
    path: String,
    /// Offset of the next chunk to ask the runner for.
    offset: u64,
    size: u64,
    mtime_unix: i64,
    /// The first chunk, fetched before the headers were committed.
    first: Option<Vec<u8>>,
}

/// The body stream: the already-fetched first chunk, then one runner call
/// per remaining chunk, each started only when the previous one was taken.
fn chunks(pull: Pull) -> impl futures::Stream<Item = Result<Bytes, std::io::Error>> {
    futures::stream::unfold(Some(pull), |state| async move {
        let mut p = state?;
        if let Some(first) = p.first.take() {
            p.offset += first.len() as u64;
            return Some((Ok(Bytes::from(first)), Some(p)));
        }
        if p.offset >= p.size {
            return None;
        }
        let want = (p.size - p.offset).min(CHUNK_MAX_BYTES as u64);
        let reply = p
            .runner
            .fs_get(&p.id, &p.root, &p.path, p.offset, want)
            .await;
        match reply {
            Ok(FsGetReply::Chunk { head, bytes })
                if head.size == p.size
                    && head.mtime_unix == p.mtime_unix
                    && bytes.len() as u64 == want =>
            {
                p.offset += want;
                Some((Ok(Bytes::from(bytes)), Some(p)))
            }
            _ => Some((
                Err(std::io::Error::other(
                    "the file changed or the runner failed mid-transfer",
                )),
                None,
            )),
        }
    })
}

/// `GET /api/terms/{id}/file`.
pub async fn file(
    State(state): State<Arc<KbHandles>>,
    Extension(identity): Extension<Identity>,
    headers: HeaderMap,
    Path(id): Path<String>,
    query: Result<Query<PathQuery>, QueryRejection>,
) -> Response<Body> {
    if let Err(e) = gate_read(&state, &headers, &identity).await {
        return e;
    }
    if !is_valid_term_id(&id) {
        return bad_request("invalid terminal id");
    }
    let (root, path) = match parse_query(query, true) {
        Ok(v) => v,
        Err(e) => return e,
    };
    let runner = runner_for(&state).await;
    let (head, bytes) = match runner
        .fs_get(&id, &root, &path, 0, CHUNK_MAX_BYTES as u64)
        .await
    {
        Err(e) => return failure(e),
        Ok(FsGetReply::Err { code, message }) => return runner_err(&code, &message),
        Ok(FsGetReply::Chunk { head, bytes }) => (head, bytes),
    };
    // The runner enforces the cap too; this is the relay's own backstop.
    if head.size > FILE_MAX_BYTES {
        return payload_too_large("file is over the 32 MiB download cap");
    }
    // The first chunk must be the whole file or a full chunk of it; anything
    // else means the file changed under the read.
    if bytes.len() as u64 != head.size.min(CHUNK_MAX_BYTES as u64) {
        return runner_bad_reply();
    }
    let name = download_name(if head.name.is_empty() {
        &path
    } else {
        &head.name
    });
    let size = head.size;
    let body = if size == 0 {
        Body::empty()
    } else {
        Body::from_stream(chunks(Pull {
            runner,
            id,
            root,
            path,
            offset: 0,
            size,
            mtime_unix: head.mtime_unix,
            first: Some(bytes),
        }))
    };
    download_response(size, &name, body)
}

/// The one shape every download answer has.
fn download_response(size: u64, name: &str, body: Body) -> Response<Body> {
    let mut resp = Response::new(body);
    *resp.status_mut() = StatusCode::OK;
    let h = resp.headers_mut();
    h.insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("application/octet-stream"),
    );
    if let Ok(v) = HeaderValue::from_str(&content_disposition(name)) {
        h.insert(header::CONTENT_DISPOSITION, v);
    }
    h.insert(
        header::X_CONTENT_TYPE_OPTIONS,
        HeaderValue::from_static("nosniff"),
    );
    h.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    h.insert(header::CONTENT_LENGTH, HeaderValue::from(size));
    resp
}

/// The sanitised download file name for `raw` (a file name or a path):
/// its final component with path separators, control and format characters
/// (bidi overrides, zero-width), quotes and backslashes removed, at most
/// [`DOWNLOAD_NAME_MAX_BYTES`] bytes, and `download` when nothing usable is
/// left (also for `.` and `..`).
pub(crate) fn download_name(raw: &str) -> String {
    let last = raw.rsplit(['/', '\\']).next().unwrap_or_default();
    let mut name: String = last
        .chars()
        .filter(|c| {
            !c.is_control()
                && !matches!(
                    *c,
                    '"' | '\\'
                        | '/'
                        | '\u{200B}'..='\u{200F}'
                        | '\u{202A}'..='\u{202E}'
                        | '\u{2066}'..='\u{2069}'
                        | '\u{FEFF}'
                )
        })
        .collect();
    while name.len() > DOWNLOAD_NAME_MAX_BYTES {
        name.pop();
    }
    let name = name.trim().to_string();
    if name.is_empty() || name == "." || name == ".." {
        "download".to_string()
    } else {
        name
    }
}

/// `attachment; filename="<ascii>"; filename*=UTF-8''<pct>` for an
/// already sanitised `name`. The plain `filename` keeps ASCII only (every
/// other character becomes `_`, as does `%` and `;`); the extended one
/// carries the exact UTF-8 name, percent-encoded outside RFC 5987's
/// `attr-char`.
pub(crate) fn content_disposition(name: &str) -> String {
    let ascii: String = name
        .chars()
        .map(|c| {
            if (c.is_ascii_graphic() && !matches!(c, '%' | ';' | '"' | '\\')) || c == ' ' {
                c
            } else {
                '_'
            }
        })
        .collect();
    let mut pct = String::with_capacity(name.len() * 3);
    for b in name.bytes() {
        let attr_char = b.is_ascii_alphanumeric()
            || matches!(
                b,
                b'!' | b'#' | b'$' | b'&' | b'+' | b'-' | b'.' | b'^' | b'_' | b'`' | b'|' | b'~'
            );
        if attr_char {
            pct.push(b as char);
        } else {
            pct.push_str(&format!("%{b:02X}"));
        }
    }
    format!("attachment; filename=\"{ascii}\"; filename*=UTF-8''{pct}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn download_names_are_a_sanitised_final_component() {
        for (raw, want) in [
            ("report.pdf", "report.pdf"),
            ("../a\"b.html", "ab.html"),
            ("dir/sub/x.txt", "x.txt"),
            ("..\\..\\evil.exe", "evil.exe"),
            ("a\u{0}b\u{7}c\n.txt", "abc.txt"),
            ("\u{202E}txt.exe", "txt.exe"),
            ("zero\u{200B}width.txt", "zerowidth.txt"),
            ("", "download"),
            ("..", "download"),
            (".", "download"),
            ("dir/", "download"),
            ("   ", "download"),
            ("\"\"", "download"),
            ("caffè è.txt", "caffè è.txt"),
            (".env", ".env"),
        ] {
            assert_eq!(download_name(raw), want, "{raw:?}");
        }
        let long = "x".repeat(500);
        assert_eq!(download_name(&long).len(), DOWNLOAD_NAME_MAX_BYTES);
        // The cut lands on a character boundary.
        let wide = "è".repeat(100);
        let cut = download_name(&wide);
        assert!(cut.len() <= DOWNLOAD_NAME_MAX_BYTES && cut.chars().all(|c| c == 'è'));
    }

    #[test]
    fn content_disposition_is_attachment_with_an_ascii_and_a_utf8_name() {
        assert_eq!(
            content_disposition("report.pdf"),
            "attachment; filename=\"report.pdf\"; filename*=UTF-8''report.pdf"
        );
        assert_eq!(
            content_disposition("caffè 50%;x.txt"),
            "attachment; filename=\"caff_ 50__x.txt\"; filename*=UTF-8''caff%C3%A8%2050%25%3Bx.txt"
        );
        // Whatever the name, the value is one valid header and starts `attachment`.
        for raw in ["a\"b", "x\r\ny", "\u{202E}", "é", "a b;c"] {
            let v = content_disposition(&download_name(raw));
            assert!(v.starts_with("attachment; filename=\""), "{v}");
            assert!(HeaderValue::from_str(&v).is_ok(), "{v}");
            assert!(!v.contains('\n') && !v.contains('\r'), "{v}");
        }
    }
}
