//! `GET /api/kb/{kb}/review/{id}` + `POST .../review/{id}/export` —
//! per-artifact comment reads (kb-comments/1). Topic 11 §B.3.
//!
//! **GET** returns the on-disk file, or — since W1.D (board D7) — a 200
//! with the canonical empty kb-comments/1 skeleton when none exists yet
//! (no-comments is a state, not an error; consumers used to synthesize
//! the same skeleton from a 404 and stay tolerant of older daemons).
//! Always sets an `ETag` header derived from `kb_core::review::etag_for`.
//!
//! The whole-document write POST was retired in R8: every mutation now
//! goes through the fine-grained endpoints in `routes::comments` (add /
//! reply / resolve / unresolve / edit / delete), which run the load →
//! typed-mutation → save sequence under the daemon-wide `review_lock`.
//! `GET` here stays the canonical read (initial SPA load, `/export`,
//! `kb comments show`, and the `comments.updated` SSE refetch).

use crate::middleware::error_to_problem_json;
use crate::state::KbHandles;
use axum::{
    body::Body,
    extract::{Path, Query, State},
    http::{header, HeaderValue, Response, StatusCode},
    response::IntoResponse,
    Json,
};
use kb_core::review::{self, ExportFormat, ReviewFile};
use serde::Deserialize;
use std::sync::Arc;

use super::is_safe_id;

/// Memory cap for the rendered `/export` body. v0.7 P2 dropped the v0.5
/// 1 MB cap entirely (it 413'd legit 200-comment reviews) — but with
/// nothing in its place a pathologically large review drove unbounded
/// allocation. 32 MiB is generous: a realistic review is 1-2 MB (a
/// 600-comment review is ~1.2 MB), and 32 MiB is ~16k comments — well
/// past anything a human authors — while still bounding peak memory.
/// v0.7.1 H7.
const EXPORT_MAX_BYTES: usize = 32 * 1024 * 1024;

pub async fn get(
    State(state): State<Arc<KbHandles>>,
    Path((kb, id)): Path<(String, String)>,
) -> Response<Body> {
    let (kb_name, _ctx) = match super::resolve_kb(&state, &kb) {
        Ok(v) => v,
        Err(resp) => return resp,
    };
    if !is_safe_id(&id) {
        return error_to_problem_json(&kb_core::Error::BadRequest(format!(
            "artifact id {id:?} contains illegal characters"
        )));
    }
    let path = state.paths.kb_review_file(&kb_name, &id);
    match review::load(&path) {
        Ok(Some(file)) => with_etag(file, &path),
        // D7 (W1.D) — an artifact with no comments yet is an ordinary state,
        // not an error: 200 + the canonical empty kb-comments/1 skeleton
        // (the same shape routes/artifact.rs injects and every consumer
        // already synthesized locally on the old 404). `etag_for` on a
        // missing file yields no ETag, so conditional reloads stay correct.
        Ok(None) => with_etag(ReviewFile::empty_skeleton(&kb_name, &id, ""), &path),
        Err(e) => error_to_problem_json(&e),
    }
}

fn with_etag(file: ReviewFile, path: &std::path::Path) -> Response<Body> {
    let etag = review::etag_for(path).ok().flatten();
    let mut resp = Json(file).into_response();
    if let Some(e) = etag {
        if let Ok(v) = HeaderValue::from_str(&e) {
            resp.headers_mut().insert(header::ETAG, v);
        }
    }
    // Reviews change frequently — never cache.
    resp.headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    resp
}

// --- v0.5 P3 — server-side review export ---------------------------------

#[derive(Debug, Deserialize)]
pub struct ExportQuery {
    /// `claude` (default Claude prompt) | `json` (raw kb-comments/1) |
    /// `md` (human-readable summary including resolved comments).
    pub format: Option<String>,
}

/// `POST /api/kb/{kb}/review/{id}/export?format=claude|json|md`. Streams
/// the formatted body (chunked transfer, no Content-Length), rejecting
/// with 413 above `EXPORT_MAX_BYTES`. Reuses `kb_core::review::export` —
/// the same impl kb-cli's `kb comments export` calls.
pub async fn post_export(
    State(state): State<Arc<KbHandles>>,
    Path((kb, id)): Path<(String, String)>,
    Query(params): Query<ExportQuery>,
) -> Response<Body> {
    let (kb_name, _ctx) = match super::resolve_kb(&state, &kb) {
        Ok(v) => v,
        Err(resp) => return resp,
    };
    if !is_safe_id(&id) {
        return error_to_problem_json(&kb_core::Error::BadRequest(format!(
            "artifact id {id:?} contains illegal characters"
        )));
    }
    let format = match params.format.as_deref().and_then(ExportFormat::from_query) {
        Some(f) => f,
        None => {
            return error_to_problem_json(&kb_core::Error::BadRequest(
                "?format=claude|json|md required".into(),
            ));
        }
    };
    let path = state.paths.kb_review_file(&kb_name, &id);
    let file = match review::load(&path) {
        Ok(Some(f)) => f,
        Ok(None) => {
            return error_to_problem_json(&kb_core::Error::NotFound(format!(
                "no comments yet for {kb_name}/{id}"
            )));
        }
        Err(e) => return error_to_problem_json(&e),
    };
    let body = match review::export(&file, kb_name.as_str(), format) {
        Ok(s) => s,
        Err(e) => return error_to_problem_json(&e),
    };
    // v0.7.1 H7 — bound peak memory. `review::export` buffers the whole
    // rendered body; reject anything past the cap rather than let a
    // pathological review drive unbounded allocation.
    if body.len() > EXPORT_MAX_BYTES {
        return export_too_large(body.len());
    }
    // Hand the body to a single-frame stream: chunked transfer, no
    // Content-Length, and no redundant second copy (the pre-H7 code
    // re-chunked `body` into a fresh `Vec<Vec<u8>>`, doubling the
    // footprint). `into_bytes` is a zero-cost String → Vec<u8> move.
    let stream = futures::stream::iter([Ok::<_, std::io::Error>(body.into_bytes())]);
    let mut resp = Response::new(Body::from_stream(stream));
    resp.headers_mut().insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static(format.content_type()),
    );
    if let Ok(disp) = HeaderValue::from_str(&format!(
        "attachment; filename=\"{id}-review.{}\"",
        format.extension()
    )) {
        resp.headers_mut().insert(header::CONTENT_DISPOSITION, disp);
    }
    resp
}

/// 413 problem+json for an export whose rendered body exceeds
/// `EXPORT_MAX_BYTES`. `kb_core::Error` has no payload-too-large variant,
/// so the response is built here (same shape as middleware's `forbidden`).
fn export_too_large(len: usize) -> Response<Body> {
    let body = serde_json::json!({
        "type": "urn:kb:errors:payload-too-large",
        "title": "Payload Too Large",
        "status": 413,
        "detail": format!(
            "rendered export is {len} bytes; the cap is {EXPORT_MAX_BYTES} bytes"
        ),
    });
    let mut resp = (StatusCode::PAYLOAD_TOO_LARGE, Json(body)).into_response();
    resp.headers_mut().insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("application/problem+json"),
    );
    resp
}
