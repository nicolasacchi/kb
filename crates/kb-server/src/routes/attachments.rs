//! Y-track — comment/reply attachment HTTP routes.
//!
//!   POST   …/review/{id}/attachments                          stage upload(s)
//!   GET    …/review/{id}/attachments/{aid}                    serve a blob
//!   POST   …/review/{id}/comments/{cid}/attachments           upload+adopt → comment
//!   POST   …/review/{id}/comments/{cid}/replies/{rid}/attachments  upload+adopt → reply
//!   DELETE …/review/{id}/comments/{cid}/attachments/{aid}          detach (comment)
//!   DELETE …/review/{id}/comments/{cid}/replies/{rid}/attachments/{aid}  detach (reply)
//!
//! The storage + security model lives in `kb_core::attachments` (the
//! magic-byte sniff that gates uploads, the manifest, the GC). This module
//! is the HTTP edge: a multipart read with a streaming size cap, and every
//! blob + manifest + review-file mutation under ONE acquisition of the
//! per-kb `review_lock` (the SAME lock that guards the review file — root
//! invariant #6), with the XSS-safe serve headers (invariant #18). Blobs
//! are written atomically BEFORE the manifest is saved, so a reader that
//! sees a manifest entry always finds its blob.
//!
//! `adopt_staged`, `gc_manifest`, and `attachment_limits` are `pub(crate)`
//! so the create-time adoption in `routes::comments` (add_comment /
//! add_reply / delete_comment) reuses them under its own review_lock guard.

use crate::middleware::error_to_problem_json;
use crate::routes::comments::{check_subid, emit_updated, validate};
use crate::state::KbHandles;
use axum::{
    body::Body,
    extract::{Multipart, Path, State},
    http::{header, Response, StatusCode},
    response::IntoResponse,
    Extension, Json,
};
use kb_core::attachments::{self, Manifest, ManifestEntry};
use kb_core::review::{self, new_attachment_id, Attachment, Author, ReviewFile};
use kb_core::types::KbName;
use serde::Serialize;
use std::collections::HashSet;
use std::sync::Arc;

/// Multipart-envelope slack added to the per-file cap for the outer
/// `DefaultBodyLimit` (router layer). The precise per-file enforcement is
/// the streaming cap in [`drain_files`]; this is the coarse outer backstop.
pub const BODY_LIMIT_SLACK: u64 = 1 << 20; // 1 MiB

/// A prepared-but-not-yet-written blob: `(aid, bytes, manifest entry)`.
type PreparedBlob = (String, Vec<u8>, ManifestEntry);

#[derive(Serialize)]
struct StagedResponse {
    #[serde(flatten)]
    attachment: Attachment,
    /// Convenience serve URL (the SPA can also build this from kb/id/aid).
    url: String,
}

// --- shared helpers --------------------------------------------------------

/// RFC-7807 problem+json for a status not represented by a `kb_core::Error`
/// variant (413 / 415). Mirrors `middleware::error_to_problem_json`.
fn problem(status: StatusCode, detail: impl Into<String>) -> Response<Body> {
    let body = serde_json::json!({
        "type": "about:blank",
        "title": status.canonical_reason().unwrap_or("Error"),
        "status": status.as_u16(),
        "detail": detail.into(),
    });
    let mut resp = (status, Json(body)).into_response();
    resp.headers_mut().insert(
        header::CONTENT_TYPE,
        header::HeaderValue::from_static("application/problem+json"),
    );
    resp
}

fn parse_author(s: &str) -> Author {
    if s.trim().eq_ignore_ascii_case("claude") {
        Author::Claude
    } else {
        Author::You
    }
}

fn serve_url(kb: &KbName, id: &str, aid: &str) -> String {
    format!("/api/kb/{}/review/{}/attachments/{}", kb.as_str(), id, aid)
}

/// Resolve `(max_file_bytes, max_per_comment, gc_grace_hours)` from the
/// running config (defaults when `[server.attachments]` is absent).
pub(crate) async fn attachment_limits(state: &KbHandles) -> (u64, usize, i64) {
    let cfg = state.config.read().await;
    let a = cfg.server.attachments.clone().unwrap_or_default();
    (a.max_file_bytes(), a.max_per_comment(), a.gc_grace_hours())
}

/// Drain a multipart request into `(author, files)` with the per-file
/// streaming size cap. `Err` is a rendered problem+json (malformed stream,
/// oversize → 413, too many files → 400).
#[allow(clippy::result_large_err)]
async fn drain_files(
    multipart: &mut Multipart,
    max_bytes: u64,
    max_count: usize,
) -> Result<(Author, Vec<(Option<String>, Vec<u8>)>), Response<Body>> {
    let mut author = Author::You;
    let mut files: Vec<(Option<String>, Vec<u8>)> = Vec::new();
    loop {
        let mut field = match multipart.next_field().await {
            Ok(Some(f)) => f,
            Ok(None) => break,
            Err(e) => {
                return Err(problem(
                    StatusCode::BAD_REQUEST,
                    format!("malformed multipart: {e}"),
                ))
            }
        };
        let name = field.name().map(|s| s.to_string());
        let fname = field.file_name().map(|s| s.to_string());
        if name.as_deref() == Some("author") {
            if let Ok(t) = field.text().await {
                author = parse_author(&t);
            }
            continue;
        }
        let mut buf: Vec<u8> = Vec::new();
        loop {
            match field.chunk().await {
                Ok(Some(chunk)) => {
                    if buf.len() as u64 + chunk.len() as u64 > max_bytes {
                        return Err(problem(
                            StatusCode::PAYLOAD_TOO_LARGE,
                            format!("attachment exceeds the {max_bytes}-byte limit"),
                        ));
                    }
                    buf.extend_from_slice(chunk.as_ref());
                }
                Ok(None) => break,
                Err(e) => {
                    return Err(problem(
                        StatusCode::BAD_REQUEST,
                        format!("upload read failed: {e}"),
                    ))
                }
            }
        }
        if !buf.is_empty() {
            files.push((fname, buf));
        }
        if files.len() > max_count {
            return Err(problem(
                StatusCode::BAD_REQUEST,
                format!("too many files in one upload (max {max_count})"),
            ));
        }
    }
    Ok((author, files))
}

/// Sniff + sanitize each drained file (disallowed type → 415), minting an
/// `aid` and building a `ManifestEntry` with the given `adopted` flag.
/// Done before any disk write, so a bad part fails the request atomically.
#[allow(clippy::result_large_err)]
fn prepare_entries(
    files: Vec<(Option<String>, Vec<u8>)>,
    author: Author,
    adopted: bool,
) -> Result<Vec<PreparedBlob>, Response<Body>> {
    let now = chrono::Utc::now();
    let mut prepared = Vec::with_capacity(files.len());
    for (fname, buf) in files {
        let Some(ct) = attachments::sniff_allowed(&buf) else {
            return Err(problem(
                StatusCode::UNSUPPORTED_MEDIA_TYPE,
                "unsupported attachment type (allowed: PNG, JPEG, GIF, WEBP, PDF, UTF-8 text)",
            ));
        };
        let entry = ManifestEntry {
            filename: attachments::sanitize_filename(fname.as_deref().unwrap_or("file")),
            content_type: ct.to_string(),
            size: buf.len() as u64,
            created_at: now,
            author,
            adopted,
        };
        prepared.push((new_attachment_id(), buf, entry));
    }
    Ok(prepared)
}

/// Write each prepared blob atomically; on any failure, roll back the blobs
/// written in this batch so a mid-batch failure leaves no un-manifested
/// orphan.
fn write_blobs(
    state: &KbHandles,
    kb: &KbName,
    id: &str,
    prepared: &[PreparedBlob],
) -> std::io::Result<()> {
    let mut written: Vec<&str> = Vec::new();
    for (aid, buf, _) in prepared {
        let blob = state.paths.kb_attachment_blob(kb, id, aid);
        if let Err(e) = write_blob_atomic(&blob, buf) {
            for waid in &written {
                let _ = std::fs::remove_file(state.paths.kb_attachment_blob(kb, id, waid));
            }
            return Err(e);
        }
        written.push(aid);
    }
    Ok(())
}

fn remove_blobs(state: &KbHandles, kb: &KbName, id: &str, prepared: &[PreparedBlob]) {
    for (aid, _, _) in prepared {
        let _ = std::fs::remove_file(state.paths.kb_attachment_blob(kb, id, aid));
    }
}

/// Adopt staged attachment ids → `Attachment`s, flipping each manifest
/// entry's `adopted` flag. `Err(BadRequest)` on an unknown/expired id or
/// when the per-target cap would be exceeded. Pure (caller persists the
/// manifest). Reused by the create-time adoption in `routes::comments`.
pub(crate) fn adopt_staged(
    manifest: &mut Manifest,
    ids: &[String],
    existing_count: usize,
    max_per_target: usize,
    user: Option<&str>,
) -> kb_core::Result<Vec<Attachment>> {
    if existing_count + ids.len() > max_per_target {
        return Err(kb_core::Error::BadRequest(format!(
            "too many attachments (max {max_per_target} per comment/reply)"
        )));
    }
    let mut out = Vec::with_capacity(ids.len());
    for aid in ids {
        let entry = manifest.items.get_mut(aid).ok_or_else(|| {
            kb_core::Error::BadRequest(format!("unknown or expired attachment {aid}"))
        })?;
        entry.adopted = true;
        let mut att = entry.to_attachment(aid);
        // v0.34 Y1: attribution — the adopting request's identity (attach
        // itself stays open to every user per the mutation policy).
        att.user = user.map(str::to_string);
        out.push(att);
    }
    Ok(out)
}

/// GC the manifest after a review mutation: reap adopted-orphan + expired-
/// staged blobs (keeping everything `referenced` by a live comment/reply),
/// then save the pruned manifest. Best-effort — logs on failure, never
/// fails the request (the review mutation already succeeded + saved). MUST
/// be called under the per-kb `review_lock`. Reused by `routes::comments`.
pub(crate) fn gc_manifest(
    state: &KbHandles,
    kb: &KbName,
    id: &str,
    manifest: Manifest,
    referenced: &HashSet<String>,
    grace_hours: i64,
) {
    let now = chrono::Utc::now();
    let (to_delete, pruned) = attachments::gc_plan(
        &manifest,
        referenced,
        now,
        chrono::Duration::hours(grace_hours),
    );
    for aid in &to_delete {
        let _ = std::fs::remove_file(state.paths.kb_attachment_blob(kb, id, aid));
    }
    if let Err(e) = attachments::save_manifest(&state.paths.kb_attachment_manifest(kb, id), &pruned)
    {
        tracing::warn!(kb=%kb, %id, error=%e, "failed to save attachment manifest after GC");
    }
}

/// Atomic blob write (tmp sibling + rename), so a concurrent `serve` can
/// never read a half-written blob.
fn write_blob_atomic(path: &std::path::Path, bytes: &[u8]) -> std::io::Result<()> {
    let parent = path.parent().unwrap_or_else(|| std::path::Path::new("."));
    let tmp = parent.join(format!(
        "{}.tmp.{}",
        path.file_name().and_then(|s| s.to_str()).unwrap_or("blob"),
        std::process::id()
    ));
    std::fs::write(&tmp, bytes)?;
    std::fs::rename(&tmp, path)
}

// --- stage (compose-time, no review mutation) ------------------------------

/// `POST …/review/{id}/attachments` — stage one or more uploads for later
/// adoption (the SPA compose-time path: stage → get `aid`+url → drop
/// `![](attachment:aid)` into the draft → `addComment {attachment_ids}`
/// adopts). The blob lands as a `staged` manifest entry, GC-reaped after the
/// grace window if never adopted.
pub async fn stage(
    State(state): State<Arc<KbHandles>>,
    Path((kb, id)): Path<(String, String)>,
    mut multipart: Multipart,
) -> Response<Body> {
    let kb_name = match validate(&state, &kb, &id) {
        Ok(k) => k,
        Err(resp) => return resp,
    };
    let (max_bytes, max_count, grace_hours) = attachment_limits(&state).await;
    let (author, files) = match drain_files(&mut multipart, max_bytes, max_count).await {
        Ok(v) => v,
        Err(resp) => return resp,
    };
    if files.is_empty() {
        return problem(StatusCode::BAD_REQUEST, "no file part in the upload");
    }
    let prepared = match prepare_entries(files, author, false) {
        Ok(v) => v,
        Err(resp) => return resp,
    };

    if let Err(e) = std::fs::create_dir_all(state.paths.kb_attachment_dir(&kb_name, &id)) {
        return error_to_problem_json(&kb_core::Error::Io(e));
    }
    let lock = state.review_lock_for(&kb_name);
    let guard = lock.lock().await;

    let manifest_path = state.paths.kb_attachment_manifest(&kb_name, &id);
    let mut manifest = attachments::load_manifest(&manifest_path);

    // Opportunistic prune of abandoned staged uploads only: treat every
    // adopted entry as referenced so this never reaps one (the full
    // reference-counted GC, which needs the review file, runs on
    // adopt/detach/delete).
    let keep_adopted: HashSet<String> = manifest
        .items
        .iter()
        .filter(|(_, e)| e.adopted)
        .map(|(k, _)| k.clone())
        .collect();
    let now = chrono::Utc::now();
    let (stale, pruned) = attachments::gc_plan(
        &manifest,
        &keep_adopted,
        now,
        chrono::Duration::hours(grace_hours),
    );
    for aid in &stale {
        let _ = std::fs::remove_file(state.paths.kb_attachment_blob(&kb_name, &id, aid));
    }
    manifest = pruned;

    if let Err(e) = write_blobs(&state, &kb_name, &id, &prepared) {
        drop(guard);
        return error_to_problem_json(&kb_core::Error::Io(e));
    }
    let mut out: Vec<StagedResponse> = Vec::with_capacity(prepared.len());
    for (aid, _buf, entry) in &prepared {
        manifest.items.insert(aid.clone(), entry.clone());
        out.push(StagedResponse {
            attachment: entry.to_attachment(aid),
            url: serve_url(&kb_name, &id, aid),
        });
    }
    if let Err(e) = attachments::save_manifest(&manifest_path, &manifest) {
        remove_blobs(&state, &kb_name, &id, &prepared);
        drop(guard);
        return error_to_problem_json(&e);
    }
    drop(guard);
    (StatusCode::CREATED, Json(out)).into_response()
}

// --- serve -----------------------------------------------------------------

/// `GET …/review/{id}/attachments/{aid}` — serve a blob. The XSS guard
/// (root invariant #18): `Content-Type` is the daemon's stored magic-byte
/// sniff (never the client's), `X-Content-Type-Options: nosniff` is ALWAYS
/// set, and only raster images are served inline — every other type is
/// forced to download. Long, immutable cache (the `aid` is random).
pub async fn serve(
    State(state): State<Arc<KbHandles>>,
    Path((kb, id, aid)): Path<(String, String, String)>,
) -> Response<Body> {
    let kb_name = match validate(&state, &kb, &id) {
        Ok(k) => k,
        Err(resp) => return resp,
    };
    if let Err(resp) = check_subid(&aid, "attachment id") {
        return resp;
    }

    let manifest = attachments::load_manifest(&state.paths.kb_attachment_manifest(&kb_name, &id));
    let Some(entry) = manifest.items.get(&aid) else {
        return error_to_problem_json(&kb_core::Error::NotFound(format!(
            "attachment {aid} not found"
        )));
    };

    let bytes = match std::fs::read(state.paths.kb_attachment_blob(&kb_name, &id, &aid)) {
        Ok(b) => b,
        Err(_) => {
            return error_to_problem_json(&kb_core::Error::NotFound(format!(
                "attachment {aid} blob missing"
            )))
        }
    };

    let disposition = if attachments::is_inline_image(&entry.content_type) {
        "inline".to_string()
    } else {
        format!("attachment; filename=\"{}\"", entry.filename)
    };

    Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, entry.content_type.clone())
        // nosniff is the keystone of the XSS guard: the browser must not
        // re-sniff a text/pdf blob into something executable.
        .header("x-content-type-options", "nosniff")
        .header(header::CONTENT_DISPOSITION, disposition)
        .header(header::CACHE_CONTROL, "public, max-age=31536000, immutable")
        .body(Body::from(bytes))
        .unwrap_or_else(|_| {
            error_to_problem_json(&kb_core::Error::Storage("response build failed".into()))
        })
}

// --- upload + adopt to an existing comment/reply (CLI one-shot) ------------

pub async fn upload_to_comment(
    State(state): State<Arc<KbHandles>>,
    Extension(identity): Extension<crate::middleware::Identity>,
    Path((kb, id, cid)): Path<(String, String, String)>,
    multipart: Multipart,
) -> Response<Body> {
    upload_and_adopt(state, identity, kb, id, cid, None, multipart).await
}

pub async fn upload_to_reply(
    State(state): State<Arc<KbHandles>>,
    Extension(identity): Extension<crate::middleware::Identity>,
    Path((kb, id, cid, rid)): Path<(String, String, String, String)>,
    multipart: Multipart,
) -> Response<Body> {
    upload_and_adopt(state, identity, kb, id, cid, Some(rid), multipart).await
}

async fn upload_and_adopt(
    state: Arc<KbHandles>,
    identity: crate::middleware::Identity,
    kb: String,
    id: String,
    cid: String,
    rid: Option<String>,
    mut multipart: Multipart,
) -> Response<Body> {
    let kb_name = match validate(&state, &kb, &id) {
        Ok(k) => k,
        Err(resp) => return resp,
    };
    if let Err(resp) = check_subid(&cid, "comment id") {
        return resp;
    }
    if let Some(rid) = &rid {
        if let Err(resp) = check_subid(rid, "reply id") {
            return resp;
        }
    }
    let (max_bytes, max_count, grace_hours) = attachment_limits(&state).await;
    let (author, files) = match drain_files(&mut multipart, max_bytes, max_count).await {
        Ok(v) => v,
        Err(resp) => return resp,
    };
    if files.is_empty() {
        return problem(StatusCode::BAD_REQUEST, "no file part in the upload");
    }
    let prepared = match prepare_entries(files, author, true) {
        Ok(v) => v,
        Err(resp) => return resp,
    };

    if let Err(e) = std::fs::create_dir_all(state.paths.kb_attachment_dir(&kb_name, &id)) {
        return error_to_problem_json(&kb_core::Error::Io(e));
    }
    let review_path = state.paths.kb_review_file(&kb_name, &id);
    let manifest_path = state.paths.kb_attachment_manifest(&kb_name, &id);
    let lock = state.review_lock_for(&kb_name);
    let guard = lock.lock().await;

    let mut review = match review::load(&review_path) {
        Ok(Some(f)) => f,
        Ok(None) => {
            drop(guard);
            return error_to_problem_json(&kb_core::Error::NotFound(format!(
                "no comments for {kb_name}/{id}"
            )));
        }
        Err(e) => {
            drop(guard);
            return error_to_problem_json(&e);
        }
    };

    // Verify the target exists + cap-check BEFORE any disk write (so a bad
    // cid/rid never leaves an orphan blob).
    let existing = match locate_count(&review, &cid, rid.as_deref()) {
        Ok(n) => n,
        Err(e) => {
            drop(guard);
            return error_to_problem_json(&e);
        }
    };
    if existing + prepared.len() > max_count {
        drop(guard);
        return problem(
            StatusCode::BAD_REQUEST,
            format!("too many attachments (max {max_count} per comment/reply)"),
        );
    }

    if let Err(e) = write_blobs(&state, &kb_name, &id, &prepared) {
        drop(guard);
        return error_to_problem_json(&kb_core::Error::Io(e));
    }

    let mut manifest = attachments::load_manifest(&manifest_path);
    let mut out: Vec<Attachment> = Vec::with_capacity(prepared.len());
    for (aid, _buf, entry) in &prepared {
        manifest.items.insert(aid.clone(), entry.clone());
        let mut att = entry.to_attachment(aid);
        // v0.34 Y1: attribution — the uploading request's identity.
        att.user = Some(identity.user.clone());
        let res = match &rid {
            Some(rid) => review.add_reply_attachment(&cid, rid, att.clone()),
            None => review.add_comment_attachment(&cid, att.clone()),
        };
        if let Err(e) = res {
            // Existence was verified above, so this shouldn't happen — but
            // roll back this batch's blobs if it ever does.
            remove_blobs(&state, &kb_name, &id, &prepared);
            drop(guard);
            return error_to_problem_json(&e);
        }
        out.push(att);
    }

    if let Err(e) = review::save_atomic(&review_path, &review, None) {
        remove_blobs(&state, &kb_name, &id, &prepared);
        drop(guard);
        return error_to_problem_json(&e);
    }
    gc_manifest(
        &state,
        &kb_name,
        &id,
        manifest,
        &review.referenced_attachment_ids(),
        grace_hours,
    );
    drop(guard);

    emit_updated(&state, &kb_name, &id, &review);
    (StatusCode::CREATED, Json(out)).into_response()
}

/// Count the attachments currently on a comment (or one of its replies),
/// returning `Err(NotFound)` if the comment/reply is absent.
fn locate_count(review: &ReviewFile, cid: &str, rid: Option<&str>) -> kb_core::Result<usize> {
    let c = review
        .comments
        .iter()
        .find(|c| c.id == cid)
        .ok_or_else(|| kb_core::Error::NotFound(format!("comment {cid}")))?;
    match rid {
        None => Ok(c.attachments.len()),
        Some(rid) => {
            let r =
                c.replies.iter().find(|r| r.id == rid).ok_or_else(|| {
                    kb_core::Error::NotFound(format!("reply {rid} in comment {cid}"))
                })?;
            Ok(r.attachments.len())
        }
    }
}

// --- detach ----------------------------------------------------------------

pub async fn detach_comment(
    State(state): State<Arc<KbHandles>>,
    Path((kb, id, cid, aid)): Path<(String, String, String, String)>,
) -> Response<Body> {
    detach(state, kb, id, cid, None, aid).await
}

pub async fn detach_reply(
    State(state): State<Arc<KbHandles>>,
    Path((kb, id, cid, rid, aid)): Path<(String, String, String, String, String)>,
) -> Response<Body> {
    detach(state, kb, id, cid, Some(rid), aid).await
}

async fn detach(
    state: Arc<KbHandles>,
    kb: String,
    id: String,
    cid: String,
    rid: Option<String>,
    aid: String,
) -> Response<Body> {
    let kb_name = match validate(&state, &kb, &id) {
        Ok(k) => k,
        Err(resp) => return resp,
    };
    if let Err(resp) = check_subid(&cid, "comment id") {
        return resp;
    }
    if let Some(rid) = &rid {
        if let Err(resp) = check_subid(rid, "reply id") {
            return resp;
        }
    }
    if let Err(resp) = check_subid(&aid, "attachment id") {
        return resp;
    }
    let (_, _, grace_hours) = attachment_limits(&state).await;

    let review_path = state.paths.kb_review_file(&kb_name, &id);
    let lock = state.review_lock_for(&kb_name);
    let guard = lock.lock().await;

    let mut review = match review::load(&review_path) {
        Ok(Some(f)) => f,
        Ok(None) => {
            drop(guard);
            return error_to_problem_json(&kb_core::Error::NotFound(format!(
                "no comments for {kb_name}/{id}"
            )));
        }
        Err(e) => {
            drop(guard);
            return error_to_problem_json(&e);
        }
    };
    let res = match &rid {
        Some(rid) => review.remove_reply_attachment(&cid, rid, &aid),
        None => review.remove_comment_attachment(&cid, &aid),
    };
    if let Err(e) = res {
        drop(guard);
        return error_to_problem_json(&e);
    }
    if let Err(e) = review::save_atomic(&review_path, &review, None) {
        drop(guard);
        return error_to_problem_json(&e);
    }
    // The detached blob is now unreferenced → reaped by the GC.
    let manifest = attachments::load_manifest(&state.paths.kb_attachment_manifest(&kb_name, &id));
    gc_manifest(
        &state,
        &kb_name,
        &id,
        manifest,
        &review.referenced_attachment_ids(),
        grace_hours,
    );
    drop(guard);

    emit_updated(&state, &kb_name, &id, &review);
    (StatusCode::OK, Json(serde_json::json!({ "ok": true }))).into_response()
}
