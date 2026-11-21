//! `POST /api/terms/{id}/upload` — hand a picture the operator picked on a
//! phone to the session's runner, which stores it and answers a path
//! (T18).
//!
//! kb persists no pixels and reads none: the body is buffered (at most
//! [`UPLOAD_MAX_BYTES`]), relayed to the runner over its unix socket with
//! [`UnixRunner::call_with_body`], and dropped. The runner sniffs the
//! bytes (PNG, JPEG, GIF, WebP only), names the file by its sha256 inside
//! the session's own upload directory and returns the absolute path; the
//! SPA then PASTES that path (never Enter). Raw bytes cannot be scrubbed,
//! so the route lives strictly inside the terminal-tier gate: it is a
//! POST like the rest of `/api/terms`, runs the SAME [`gate`] first
//! (enabled -> exact Origin -> intent header -> caller class, no loopback
//! bypass) and nothing here re-implements any part of it.
//!
//! The body limit is two layers, because the extractor limit alone would
//! admit up to its slack over the cap: the sub-router in `router.rs`
//! carries `DefaultBodyLimit::max(UPLOAD_BODY_LIMIT)` (so a body far past
//! the cap is cut off while being read), and the handler refuses any body
//! over [`UPLOAD_MAX_BYTES`] itself. The extractor's refusal is taken as a
//! `Result` so it cannot pre-empt the gate: a caller the gate refuses
//! learns nothing about the size limit.
//!
//! Nothing here emits an SSE event, and nothing is persisted (LF-7).

use std::sync::Arc;

use axum::body::{Body, Bytes};
use axum::extract::rejection::BytesRejection;
use axum::extract::{Path, State};
use axum::http::{header, HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::Extension;

use crate::middleware::Identity;
use crate::routes::terms::{
    bad_request, no_store, payload_too_large, runner_bad_reply, runner_down, runner_err, runner_for,
};
use crate::state::KbHandles;
use crate::terminal::binding::is_valid_term_id;
use crate::terminal::gate::{gate, Problem};
use crate::terminal::media::ERR_UNSUPPORTED_MEDIA;
use crate::terminal::runner_client::RunnerError;
use crate::terminal::wire::{RunnerReply, RunnerRequest, UploadData};

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
}
