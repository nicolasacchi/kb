//! The terminal upload doors: `POST /api/terms/{id}/upload` (a picture, T18)
//! and `POST /api/terms/{id}/files` (any file, FXUP).
//!
//! Both hand bytes the operator picked in the browser to the session's
//! runner, which stores them under the session's own `up/` directory and
//! answers an absolute path; the SPA then PASTES that path (never Enter).
//! Raw bytes cannot be scrubbed, so both routes live strictly inside the
//! terminal-tier gate: a POST like the rest of `/api/terms`, running the
//! SAME [`gate`] first (enabled -> exact Origin -> intent header -> caller
//! class, no loopback bypass); nothing here re-implements any part of it.
//!
//! The picture door (`/upload`) buffers the body (at most
//! [`UPLOAD_MAX_BYTES`]), relays it in one runner call and drops it; the
//! runner sniffs PNG, JPEG, GIF and WebP only. It persists nothing in kb.
//!
//! The file door (`/files`) takes any type, up to [`FILE_MAX_BYTES`], and
//! never buffers the whole body: it relays the request stream to the
//! runner in chunks of at most [`CHUNK_MAX_BYTES`] (`upload_begin`, then
//! `upload_append` per chunk, then `upload_commit`; `upload_abort` on any
//! failure). When `[terminal.files] kb` names a corpus, the same bytes are
//! also spooled to a private temp file while they stream and, after the
//! runner commit, stored into that corpus with `kb_core::filestore`
//! (`.kbblob` original plus an HTML stub). A kb-side failure never fails
//! the upload: the runner copy exists and its path is returned. The
//! sender's declared `Content-Type` is only forwarded to the runner as a
//! hint; it is never echoed back and never used to serve anything (the
//! artifact origin refuses `.kbblob` files outright).
//!
//! The picture door's body limit is two layers, because the extractor
//! limit alone would admit up to its slack over the cap: the sub-router in
//! `router.rs` carries `DefaultBodyLimit::max(UPLOAD_BODY_LIMIT)` and the
//! handler refuses any body over [`UPLOAD_MAX_BYTES`] itself. The
//! extractor's refusal is taken as a `Result` so it cannot pre-empt the
//! gate: a caller the gate refuses learns nothing about the size limit.
//!
//! Nothing here emits an SSE event, and kb persists nothing outside the
//! optional files corpus (LF-7).

use std::sync::Arc;

use axum::body::{Body, Bytes};
use axum::extract::rejection::{BytesRejection, QueryRejection};
use axum::extract::{Path, Query, State};
use axum::http::{header, HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::Extension;
use futures::StreamExt;
use serde::{Deserialize, Serialize};

use crate::middleware::Identity;
use crate::routes::terms::{
    bad_request, no_store, payload_too_large, runner_bad_reply, runner_down, runner_err, runner_for,
};
use crate::state::KbHandles;
use crate::terminal::binding::is_valid_term_id;
use crate::terminal::gate::{gate, Problem};
use crate::terminal::media::ERR_UNSUPPORTED_MEDIA;
use crate::terminal::runner_client::{RunnerError, UnixRunner};
use crate::terminal::wire::{
    FileData, RunnerReply, RunnerRequest, UploadAppendData, UploadBeginData, UploadData,
};

/// The largest picture the route accepts. ONE place: the runner holds the
/// same number (`kb_term::runner::upload::UPLOAD_MAX_BYTES`) and refuses a
/// larger `len` without reading the body.
pub(crate) const UPLOAD_MAX_BYTES: usize = 8 * 1024 * 1024;

/// The extractor-level limit: the cap plus slack, so a body that is merely
/// over the cap reaches the handler's own (precise) check.
pub(crate) const UPLOAD_BODY_LIMIT: usize = UPLOAD_MAX_BYTES + 4096;

/// The picture types the route accepts, as the request `Content-Type`.
pub(crate) const UPLOAD_TYPES: [&str; 4] = ["image/png", "image/jpeg", "image/gif", "image/webp"];

/// The request `Content-Type` reduced to its lowercase media type, if it is
/// one of [`UPLOAD_TYPES`] (parameters after `;` are ignored).
fn declared_type(headers: &HeaderMap) -> Option<&'static str> {
    let raw = headers.get(header::CONTENT_TYPE)?.to_str().ok()?;
    let essence = raw.split(';').next()?.trim().to_ascii_lowercase();
    UPLOAD_TYPES.iter().copied().find(|t| *t == essence)
}

fn unsupported_media(detail: &str) -> Response<Body> {
    no_store(Problem::response(
        ERR_UNSUPPORTED_MEDIA,
        StatusCode::UNSUPPORTED_MEDIA_TYPE,
        "Unsupported Media Type",
        detail.to_string(),
    ))
}

pub async fn terms_upload(
    State(state): State<Arc<KbHandles>>,
    Extension(identity): Extension<Identity>,
    headers: HeaderMap,
    Path(id): Path<String>,
    body: Result<Bytes, BytesRejection>,
) -> Response<Body> {
    if let Err(e) = gate(&state, &headers, &identity).await {
        return e;
    }
    if !is_valid_term_id(&id) {
        return bad_request("invalid terminal id");
    }
    let Some(mime) = declared_type(&headers) else {
        return unsupported_media(
            "upload Content-Type must be image/png, image/jpeg, image/gif or image/webp",
        );
    };
    let body = match body {
        Ok(b) => b,
        // The extractor limit (a body far past the cap) or a read failure.
        Err(rejection) if rejection.status() == StatusCode::PAYLOAD_TOO_LARGE => {
            return payload_too_large("upload over 8 MiB");
        }
        Err(_) => return bad_request("upload body could not be read"),
    };
    if body.len() > UPLOAD_MAX_BYTES {
        return payload_too_large("upload over 8 MiB");
    }
    let req = RunnerRequest::Upload {
        id: id.clone(),
        mime: mime.to_string(),
        len: body.len() as u64,
    };
    let runner = runner_for(&state).await;
    match runner.call_with_body(&req, &body).await {
        Err(RunnerError::Unavailable | RunnerError::Timeout) => runner_down(),
        Err(RunnerError::TooLarge | RunnerError::BadReply) => runner_bad_reply(),
        Ok(RunnerReply::Err { code, message }) => runner_err(&code, &message),
        Ok(RunnerReply::Ok { data }) => match serde_json::from_value::<UploadData>(data) {
            Ok(stored) => no_store(axum::Json(stored).into_response()),
            Err(_) => runner_bad_reply(),
        },
    }
}

// ---------------------------------------------------------------- files (FXUP)

/// The largest file the `/files` door accepts. The runner holds the same
/// number and refuses a larger `begin` itself.
pub(crate) const FILE_MAX_BYTES: u64 = 32 * 1024 * 1024;

/// The largest chunk relayed in one runner `upload_append` (mirrors the
/// runner's `chunk_max`; the runner's own figure wins when it is smaller).
pub(crate) const CHUNK_MAX_BYTES: usize = 8 * 1024 * 1024;

/// The extractor-level limit of the `/files` sub-router: the cap plus
/// slack, so a body merely over the cap reaches the handler's own check.
pub(crate) const FILE_BODY_LIMIT: usize = FILE_MAX_BYTES as usize + 4096;

/// `urn:kb:errors:terminal-upload-quota` — the session's `up/` quota is
/// full (413). Distinct from `payload-too-large`, which means THIS file is
/// over the per-file cap.
pub(crate) const ERR_TERMINAL_UPLOAD_QUOTA: &str = "urn:kb:errors:terminal-upload-quota";

/// Concurrent keep-into-kb steps (each transiently holds one whole file).
static KEEP_PERMITS: tokio::sync::Semaphore = tokio::sync::Semaphore::const_new(2);

// `[terminal.files]` kb-side outcome of one upload (plain comment: a doc
// comment would be copied into the generated TS binding).
#[cfg_attr(feature = "ts-export", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Clone, Serialize)]
pub struct KbKeep {
    // `stored`, `skipped` or `failed`.
    pub status: String,
    // Why it was skipped or failed (`not-configured`, `quota`, `io`).
    pub reason: Option<String>,
    // The kb artifact id of the stub, when stored.
    pub id: Option<String>,
    // The stub's path relative to the files corpus, when stored.
    pub stub_rel: Option<String>,
    // False when a byte-identical file was already stored.
    pub created: Option<bool>,
    // Whether the text content was inlined into the stub.
    pub inlined: Option<bool>,
}

impl KbKeep {
    fn skipped(reason: &str) -> Self {
        Self {
            status: "skipped".to_string(),
            reason: Some(reason.to_string()),
            id: None,
            stub_rel: None,
            created: None,
            inlined: None,
        }
    }

    fn failed(reason: &str) -> Self {
        Self {
            status: "failed".to_string(),
            ..Self::skipped(reason)
        }
    }
}

// The `/files` answer: the runner path to paste plus the kb outcome. The
// sender's declared mime is never echoed: `mime` is the runner's SNIFFED
// type for a picture and `null` for everything else.
#[cfg_attr(feature = "ts-export", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Clone, Serialize)]
pub struct FileUploadAnswer {
    pub path: String,
    pub name: String,
    pub bytes: u64,
    pub kind: String,
    pub sha256: String,
    pub mime: Option<String>,
    pub kb: KbKeep,
}

/// The `/files` query.
#[derive(Debug, Deserialize)]
pub struct FilesQuery {
    name: Option<String>,
    index: Option<String>,
}

fn quota_full() -> Response<Body> {
    no_store(Problem::response(
        ERR_TERMINAL_UPLOAD_QUOTA,
        StatusCode::PAYLOAD_TOO_LARGE,
        "Payload Too Large",
        "the terminal's upload area is full".to_string(),
    ))
}

/// The declared mime, reduced to a conservative token (it is only a hint
/// to the runner); anything odd becomes `application/octet-stream`.
fn declared_mime(headers: &HeaderMap) -> String {
    const FALLBACK: &str = "application/octet-stream";
    let Some(raw) = headers
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
    else {
        return FALLBACK.to_string();
    };
    let essence = raw
        .split(';')
        .next()
        .unwrap_or_default()
        .trim()
        .to_ascii_lowercase();
    let ok = !essence.is_empty()
        && essence.len() <= 100
        && essence.matches('/').count() == 1
        && essence
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'/' | b'.' | b'+' | b'-' | b'_'));
    if ok {
        essence
    } else {
        FALLBACK.to_string()
    }
}

/// `name` must be 1..=255 bytes with no NUL. (The runner slugs it; the kb
/// store re-sanitises it; this only refuses the unrepresentable.)
fn valid_name(name: &str) -> bool {
    !name.is_empty() && name.len() <= 255 && !name.contains('\0')
}

/// A private spool file the streamed bytes are teed into for the kb keep.
/// Removed on drop (success, error or a dropped request).
struct SpoolGuard(std::path::PathBuf);

impl Drop for SpoolGuard {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

struct Spool {
    guard: SpoolGuard,
    file: tokio::fs::File,
}

async fn open_spool(state: &Arc<KbHandles>) -> Option<Spool> {
    use std::sync::atomic::{AtomicU64, Ordering};
    static SEQ: AtomicU64 = AtomicU64::new(0);
    let dir = state.paths.cache.join("term-upload-spool");
    tokio::fs::create_dir_all(&dir).await.ok()?;
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let path = dir.join(format!(
        "{}-{nanos}-{}.spool",
        std::process::id(),
        SEQ.fetch_add(1, Ordering::Relaxed)
    ));
    let file = tokio::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&path)
        .await
        .ok()?;
    Some(Spool {
        guard: SpoolGuard(path),
        file,
    })
}

/// The files corpus source root, when `[terminal.files] kb` names a loaded kb.
async fn files_target(state: &Arc<KbHandles>) -> Option<std::path::PathBuf> {
    let cfg = state.config.read().await;
    let name = cfg.terminal.files_kb()?;
    state
        .kbs
        .iter()
        .find(|(k, _)| k.as_str() == name)
        .map(|(_, ctx)| ctx.source_path.clone())
}

/// Aborts the runner upload if the handler future is dropped (client
/// disconnect) before the upload is committed or explicitly aborted.
struct AbortOnDrop {
    runner: UnixRunner,
    id: String,
    upload: String,
    armed: bool,
}

impl Drop for AbortOnDrop {
    fn drop(&mut self) {
        if !self.armed {
            return;
        }
        if let Ok(handle) = tokio::runtime::Handle::try_current() {
            let (runner, id, upload) = (self.runner.clone(), self.id.clone(), self.upload.clone());
            handle.spawn(async move { abort_upload(&runner, &id, &upload).await });
        }
    }
}

/// Best-effort `upload_abort` (the runner's is idempotent).
async fn abort_upload(runner: &UnixRunner, id: &str, upload: &str) {
    let _ = runner
        .call(&RunnerRequest::UploadAbort {
            id: id.to_string(),
            upload: upload.to_string(),
        })
        .await;
}

fn map_runner_failure(e: RunnerError) -> Response<Body> {
    match e {
        RunnerError::Unavailable | RunnerError::Timeout => runner_down(),
        RunnerError::TooLarge | RunnerError::BadReply => runner_bad_reply(),
    }
}

/// Send one chunk; the reply must acknowledge exactly the bytes sent.
#[allow(clippy::result_large_err)]
async fn send_chunk(
    runner: &UnixRunner,
    id: &str,
    upload: &str,
    offset: u64,
    chunk: &[u8],
    spool: &mut Option<Spool>,
) -> Result<(), Response<Body>> {
    use tokio::io::AsyncWriteExt;
    if let Some(sp) = spool.as_mut() {
        if sp.file.write_all(chunk).await.is_err() {
            *spool = None;
        }
    }
    let req = RunnerRequest::UploadAppend {
        id: id.to_string(),
        upload: upload.to_string(),
        offset,
        len: chunk.len() as u64,
    };
    match runner.call_with_body(&req, chunk).await {
        Err(e) => Err(map_runner_failure(e)),
        Ok(RunnerReply::Err { code, message }) => Err(runner_err(&code, &message)),
        Ok(RunnerReply::Ok { data }) => match serde_json::from_value::<UploadAppendData>(data) {
            Ok(a) if a.received == offset + chunk.len() as u64 => Ok(()),
            _ => Err(runner_bad_reply()),
        },
    }
}

/// Relay the request stream to an open runner upload and commit it.
#[allow(clippy::too_many_arguments, clippy::result_large_err)]
async fn relay_and_commit(
    runner: &UnixRunner,
    id: &str,
    upload: &str,
    chunk_size: usize,
    declared: u64,
    body: Body,
    spool: &mut Option<Spool>,
) -> Result<FileData, Response<Body>> {
    let mut stream = body.into_data_stream();
    let mut buf: Vec<u8> = Vec::with_capacity(chunk_size.min(declared as usize));
    let mut sent: u64 = 0;
    let mut total: u64 = 0;
    while let Some(item) = stream.next().await {
        let bytes = item.map_err(|_| bad_request("upload body could not be read"))?;
        total += bytes.len() as u64;
        if total > declared {
            return Err(bad_request("upload body is longer than Content-Length"));
        }
        buf.extend_from_slice(&bytes);
        while buf.len() >= chunk_size {
            let chunk: Vec<u8> = buf.drain(..chunk_size).collect();
            send_chunk(runner, id, upload, sent, &chunk, spool).await?;
            sent += chunk.len() as u64;
        }
    }
    if total != declared {
        return Err(bad_request("upload body is shorter than Content-Length"));
    }
    if !buf.is_empty() {
        send_chunk(runner, id, upload, sent, &buf, spool).await?;
    }
    let req = RunnerRequest::UploadCommit {
        id: id.to_string(),
        upload: upload.to_string(),
    };
    match runner.call(&req).await {
        Err(e) => Err(map_runner_failure(e)),
        Ok(RunnerReply::Err { code, message }) => Err(runner_err(&code, &message)),
        Ok(RunnerReply::Ok { data }) => {
            serde_json::from_value::<FileData>(data).map_err(|_| runner_bad_reply())
        }
    }
}

/// Store the spooled bytes into the files corpus. Never fails the upload.
async fn keep_in_kb(
    target: Option<std::path::PathBuf>,
    spool: Option<Spool>,
    id: &str,
    name: &str,
    index: bool,
    expected: u64,
) -> KbKeep {
    use tokio::io::AsyncWriteExt;
    let Some(source_root) = target else {
        return KbKeep::skipped("not-configured");
    };
    let Some(Spool { guard, mut file }) = spool else {
        return KbKeep::failed("io");
    };
    if file.flush().await.is_err() {
        return KbKeep::failed("io");
    }
    drop(file);
    let Ok(_permit) = KEEP_PERMITS.acquire().await else {
        return KbKeep::failed("io");
    };
    let (id, name) = (id.to_string(), name.to_string());
    let now_unix = chrono::Utc::now().timestamp();
    let joined = tokio::task::spawn_blocking(move || {
        // The guard moves in so the spool is removed whatever happens.
        let guard = guard;
        let bytes = std::fs::read(&guard.0)
            .map_err(|e| kb_core::filestore::FileStoreError::Io(e.to_string()))?;
        if bytes.len() as u64 != expected {
            return Err(kb_core::filestore::FileStoreError::Io(
                "spool length mismatch".to_string(),
            ));
        }
        kb_core::filestore::store_file(kb_core::filestore::StoreInput {
            source_root: &source_root,
            term_id: &id,
            original_name: &name,
            bytes: &bytes,
            tags: &[],
            index_content: index,
            scrub: None,
            now_unix,
            quota_bytes: kb_core::filestore::FILES_QUOTA_BYTES,
        })
    })
    .await;
    match joined {
        Ok(Ok(stored)) => KbKeep {
            status: "stored".to_string(),
            reason: None,
            id: Some(stored.id.as_str().to_string()),
            stub_rel: Some(stored.stub_rel),
            created: Some(stored.created),
            inlined: Some(stored.inlined),
        },
        Ok(Err(kb_core::filestore::FileStoreError::Quota { .. })) => KbKeep::skipped("quota"),
        Ok(Err(_)) | Err(_) => {
            // No file name or content in the log line.
            tracing::warn!("terminal file keep into the files corpus failed");
            KbKeep::failed("io")
        }
    }
}

pub async fn terms_files(
    State(state): State<Arc<KbHandles>>,
    Extension(identity): Extension<Identity>,
    headers: HeaderMap,
    Path(id): Path<String>,
    query: Result<Query<FilesQuery>, QueryRejection>,
    body: Body,
) -> Response<Body> {
    if let Err(e) = gate(&state, &headers, &identity).await {
        return e;
    }
    if !is_valid_term_id(&id) {
        return bad_request("invalid terminal id");
    }
    let Ok(Query(query)) = query else {
        return bad_request("invalid query");
    };
    let name = match query.name {
        Some(n) if valid_name(&n) => n,
        _ => return bad_request("name is required (1-255 bytes, no NUL)"),
    };
    let index = match query.index.as_deref() {
        None | Some("0") => false,
        Some("1") => true,
        Some(_) => return bad_request("index must be 0 or 1"),
    };
    let declared = match headers
        .get(header::CONTENT_LENGTH)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.trim().parse::<u64>().ok())
    {
        Some(n) => n,
        None => return bad_request("length required"),
    };
    if declared > FILE_MAX_BYTES {
        return payload_too_large("upload over 32 MiB");
    }
    if declared == 0 {
        return bad_request("empty upload");
    }

    let runner = runner_for(&state).await;
    let begin = RunnerRequest::UploadBegin {
        id: id.clone(),
        name: name.clone(),
        mime: declared_mime(&headers),
        size: declared,
    };
    let begun = match runner.call(&begin).await {
        Err(e) => return map_runner_failure(e),
        // kb already enforced the per-file cap, so the runner's refusal
        // here is the session's `up/` quota.
        Ok(RunnerReply::Err { code, .. }) if code == "too-large" => return quota_full(),
        Ok(RunnerReply::Err { code, message }) => return runner_err(&code, &message),
        Ok(RunnerReply::Ok { data }) => match serde_json::from_value::<UploadBeginData>(data) {
            Ok(b) => b,
            Err(_) => return runner_bad_reply(),
        },
    };
    let chunk_size = (begun.chunk_max as usize).clamp(1, CHUNK_MAX_BYTES);

    let target = files_target(&state).await;
    let mut spool = if target.is_some() {
        open_spool(&state).await
    } else {
        None
    };

    let mut guard = AbortOnDrop {
        runner: runner.clone(),
        id: id.clone(),
        upload: begun.upload.clone(),
        armed: true,
    };
    let file = match relay_and_commit(
        &runner,
        &id,
        &begun.upload,
        chunk_size,
        declared,
        body,
        &mut spool,
    )
    .await
    {
        Ok(f) => {
            guard.armed = false;
            f
        }
        Err(resp) => {
            guard.armed = false;
            abort_upload(&runner, &id, &begun.upload).await;
            return resp;
        }
    };

    let kb = keep_in_kb(target, spool, &id, &name, index, file.bytes).await;
    let mime = (file.kind == "image").then(|| file.mime.clone());
    let answer = FileUploadAnswer {
        path: file.path,
        name: file.name,
        bytes: file.bytes,
        kind: file.kind,
        sha256: file.sha256,
        mime,
        kb,
    };
    let mut resp = no_store(axum::Json(answer).into_response());
    resp.headers_mut().insert(
        header::X_CONTENT_TYPE_OPTIONS,
        header::HeaderValue::from_static("nosniff"),
    );
    resp
}

#[cfg(test)]
mod tests {
    use super::*;

    fn headers(ct: Option<&str>) -> HeaderMap {
        let mut h = HeaderMap::new();
        if let Some(v) = ct {
            h.insert(header::CONTENT_TYPE, v.parse().unwrap());
        }
        h
    }

    #[test]
    fn only_the_four_picture_types_are_declared_types() {
        for ok in UPLOAD_TYPES {
            assert_eq!(declared_type(&headers(Some(ok))), Some(ok));
        }
        assert_eq!(
            declared_type(&headers(Some("Image/PNG; charset=binary"))),
            Some("image/png"),
            "case and parameters are ignored"
        );
        for bad in [
            "image/svg+xml",
            "text/html",
            "application/pdf",
            "application/json",
            "application/octet-stream",
            "multipart/form-data; boundary=x",
            "image/png2",
            "",
        ] {
            assert_eq!(declared_type(&headers(Some(bad))), None, "{bad:?}");
        }
        assert_eq!(declared_type(&headers(None)), None);
    }

    #[test]
    fn the_extractor_limit_is_slack_over_the_handler_cap() {
        assert_eq!(UPLOAD_MAX_BYTES, 8 * 1024 * 1024);
        assert_eq!(UPLOAD_BODY_LIMIT, UPLOAD_MAX_BYTES + 4096);
    }

    #[test]
    fn the_file_limits_are_pinned_literally() {
        assert_eq!(FILE_MAX_BYTES, 32 * 1024 * 1024);
        assert_eq!(CHUNK_MAX_BYTES, 8 * 1024 * 1024);
        assert_eq!(FILE_BODY_LIMIT as u64, FILE_MAX_BYTES + 4096);
    }

    #[test]
    fn the_quota_urn_is_distinct_from_the_size_urn() {
        assert_eq!(
            ERR_TERMINAL_UPLOAD_QUOTA,
            "urn:kb:errors:terminal-upload-quota"
        );
        assert_ne!(
            ERR_TERMINAL_UPLOAD_QUOTA,
            crate::terminal::gate::ERR_TERMINAL_PAYLOAD_TOO_LARGE
        );
    }

    #[test]
    fn declared_mime_is_a_conservative_token() {
        assert_eq!(declared_mime(&headers(None)), "application/octet-stream");
        assert_eq!(
            declared_mime(&headers(Some("Application/PDF; charset=x"))),
            "application/pdf"
        );
        for bad in ["a b/c", "noslash", "a/b/c", "te*xt/html", ""] {
            assert_eq!(
                declared_mime(&headers(Some(bad))),
                "application/octet-stream"
            );
        }
    }

    #[test]
    fn name_validation_bounds() {
        assert!(valid_name("a"));
        assert!(valid_name(&"x".repeat(255)));
        assert!(!valid_name(""));
        assert!(!valid_name(&"x".repeat(256)));
        assert!(!valid_name("a\0b"));
    }
}
