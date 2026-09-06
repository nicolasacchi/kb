//! U2 (v0.25 quick capture) — the HTTP edge for `kb_core::capture`'s
//! staged-upload engine (U1). Two routes share one handler core
//! ([`do_capture`]):
//!
//!   `POST /api/kb/{kb}/capture` — `files` (0..n) + `title`/`tags`/
//!   `sanitize`/`from`/`url`/`text`. Sanitize defaults OFF (opt-in).
//!
//!   `POST /capture` — the Web Share Target action (`manifest.webmanifest`'s
//!   `share_target`). Destination kb = `[server.capture].default_kb` else
//!   the first configured kb; shared `.html` FILES default sanitize ON
//!   (untrusted saved pages — everywhere else it's off); success redirects
//!   `303 See Other` to `/?captured=<kb>:<source_relative>` (percent-
//!   encoded) rather than the artifact detail — indexing is async (the
//!   watcher's next debounce), so there's no artifact to show yet.
//!
//! **TRAP**: `POST /capture` is mounted OUTSIDE the `/api` nest — the
//! browser's share-target POST hits bare `/capture` (that's the manifest's
//! `action` field, no `/api` prefix) — so it does NOT inherit the `/api`
//! tree's `auth_bearer` layer. `router.rs` applies the SAME layer to it
//! explicitly via `route_layer`; dropping that wiring silently breaks root
//! invariant #4's fail-closed guarantee on a public bind. See the
//! `capture_share_target_route_401_without_token_non_loopback` test in
//! `tests/end_to_end.rs`.
//!
//! Every file gates on the kb's RESOLVED extension map (`ctx.ext_map`, X1)
//! — the SAME map `bring_up_kb` installed on the indexer, so a capture can
//! never write a file the indexer would then refuse to pick up. An
//! unmapped extension → 415. A permission-denied write (read-only corpus,
//! e.g. the prod `:ro` bind-mount) maps to 409, not the generic 500
//! `kb_core::Error::Io` would otherwise produce.
//!
//! **Body-size budgets are two DISTINCT knobs (U2 follow-up)**: both routes
//! accept a MULTI-FILE batch (`files`, 0..[`MAX_CAPTURE_FILES`]), so
//! `[server.capture].max_file_bytes` (one file's own cap, streaming-enforced
//! in [`drain_capture_form`]) and `.max_request_bytes` (the COMBINED cap
//! across every file in the batch, checked up front against
//! `Content-Length` by [`reject_oversized_request`] before multipart
//! parsing starts) are independent — sizing the outer `DefaultBodyLimit`
//! layer (`router.rs`'s `capture_body_limit`) off `max_file_bytes` alone
//! starved a legal multi-file request even when every file was individually
//! under cap.

use crate::middleware::error_to_problem_json;
use crate::state::{KbContext, KbHandles};
use axum::{
    body::Body,
    extract::{Multipart, Path, State},
    http::{header, HeaderMap, Response, StatusCode},
    response::IntoResponse,
    Json,
};
use kb_core::capture::{self, CaptureInput, UrlStubInput};
use kb_core::extmap::Pipeline;
use kb_core::types::KbName;
use serde::Serialize;
use std::path::Path as FsPath;
use std::sync::Arc;

/// Hard cap on files per capture request — independent of the per-file byte
/// cap ([`kb_core::capture::DEFAULT_MAX_FILE_BYTES`] / `[server.capture]`)
/// AND the combined-request byte cap
/// ([`kb_core::capture::DEFAULT_MAX_REQUEST_BYTES`] /
/// `[server.capture].max_request_bytes`, U2 follow-up). Not
/// operator-configurable: no realistic capture flow needs more than this,
/// it's only a DoS backstop against a request with thousands of tiny parts
/// (each individually under the byte caps).
const MAX_CAPTURE_FILES: usize = 50;

#[cfg_attr(feature = "ts-export", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Serialize)]
pub struct CaptureItem {
    pub kb: String,
    pub id: String,
    pub source_relative: String,
    pub title: String,
    /// Echoes the request's `url` field (the shared/saved-page URL), when
    /// present — not a viewer link. Absent on a plain file capture with no
    /// `url` field.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts-export", ts(optional))]
    pub url: Option<String>,
}

#[cfg_attr(feature = "ts-export", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Serialize)]
pub struct CaptureResponse {
    pub items: Vec<CaptureItem>,
}

/// RFC-7807 problem+json for a status not represented by a `kb_core::Error`
/// variant (413/415/400 raised before any `capture()` call). Mirrors
/// `routes::attachments::problem` / `middleware::error_to_problem_json`.
pub(crate) fn problem(status: StatusCode, detail: impl Into<String>) -> Response<Body> {
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

/// `capture()`/`capture_url_stub()` surface a read-only corpus (the prod
/// `:ro` bind-mount) as a plain `Error::Io(PermissionDenied)`, which
/// `error_to_problem_json` would otherwise render as a generic 500. Map it
/// to 409 — the request is well-formed, the destination just can't be
/// written to right now.
pub(crate) fn map_capture_error(e: kb_core::Error) -> Response<Body> {
    if let kb_core::Error::Io(io_err) = &e {
        if io_err.kind() == std::io::ErrorKind::PermissionDenied {
            return error_to_problem_json(&kb_core::Error::Conflict(
                "capture destination is read-only".to_string(),
            ));
        }
    }
    error_to_problem_json(&e)
}

/// Resolve the `from:` provenance tag from an explicit multipart field,
/// falling back to the `X-Requested-By` header kb-cli/kb-spa already send
/// (stripping the `kb-` prefix so the stamped tag reads `from:cli` /
/// `from:spa`, matching `capture::CaptureInput::from`'s doc). Absent both →
/// `"api"` (a caller hitting the endpoint directly, e.g. curl).
pub(crate) fn from_default(headers: &HeaderMap) -> String {
    headers
        .get("x-requested-by")
        .and_then(|v| v.to_str().ok())
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(|s| s.strip_prefix("kb-").unwrap_or(s).to_ascii_lowercase())
        .unwrap_or_else(|| "api".to_string())
}

pub(crate) fn parse_bool_field(s: &str) -> bool {
    matches!(
        s.trim().to_ascii_lowercase().as_str(),
        "true" | "on" | "1" | "yes"
    )
}

pub(crate) fn split_tags(s: &str) -> Vec<String> {
    s.split(',')
        .map(str::trim)
        .filter(|t| !t.is_empty())
        .map(str::to_string)
        .collect()
}

pub(crate) fn non_empty(s: String) -> Option<String> {
    let t = s.trim();
    (!t.is_empty()).then(|| t.to_string())
}

/// The non-file fields + drained file bytes from one capture multipart
/// request. `sanitize` is `None` when the field was absent — the caller
/// resolves the per-route/per-pipeline default (opt-in on the API route,
/// ON for shared `.html` on the share-target route).
#[derive(Default)]
struct CaptureForm {
    title: Option<String>,
    tags: Vec<String>,
    sanitize: Option<bool>,
    from: Option<String>,
    url: Option<String>,
    text: Option<String>,
    files: Vec<(Option<String>, Vec<u8>)>,
}

/// Drain a capture multipart request: `files` parts (streaming, per-file
/// size cap — same shape as `routes::attachments::drain_files`) plus the
/// `title`/`tags`/`sanitize`/`from`/`url`/`text` text fields. `Err` is a
/// rendered problem+json (malformed stream, oversize → 413, too many files
/// → 400).
#[allow(clippy::result_large_err)]
async fn drain_capture_form(
    multipart: &mut Multipart,
    max_bytes: u64,
) -> Result<CaptureForm, Response<Body>> {
    let mut form = CaptureForm::default();
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
        let name = field.name().unwrap_or_default().to_string();
        if name == "files" {
            let fname = field.file_name().map(|s| s.to_string());
            let mut buf: Vec<u8> = Vec::new();
            loop {
                match field.chunk().await {
                    Ok(Some(chunk)) => {
                        if buf.len() as u64 + chunk.len() as u64 > max_bytes {
                            return Err(problem(
                                StatusCode::PAYLOAD_TOO_LARGE,
                                format!("capture file exceeds the {max_bytes}-byte limit"),
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
                form.files.push((fname, buf));
            }
            if form.files.len() > MAX_CAPTURE_FILES {
                return Err(problem(
                    StatusCode::BAD_REQUEST,
                    format!("too many files in one capture (max {MAX_CAPTURE_FILES})"),
                ));
            }
            continue;
        }
        let text = match field.text().await {
            Ok(t) => t,
            Err(e) => {
                return Err(problem(
                    StatusCode::BAD_REQUEST,
                    format!("malformed multipart field `{name}`: {e}"),
                ))
            }
        };
        match name.as_str() {
            "title" => form.title = non_empty(text),
            "tags" => form.tags = split_tags(&text),
            "sanitize" => form.sanitize = Some(parse_bool_field(&text)),
            "from" => form.from = non_empty(text),
            "url" => form.url = non_empty(text),
            "text" => form.text = non_empty(text),
            _ => {}
        }
    }
    Ok(form)
}

/// Resolve `(max_file_bytes, max_request_bytes, capture_dir)` for `kb_name`
/// from the running config — same `try_read`-then-default shape as
/// `routes::attachments::attachment_limits`. U2 follow-up added
/// `max_request_bytes` — the combined-request budget, distinct from the
/// per-file cap (see [`reject_oversized_request`]).
pub(crate) async fn capture_settings(state: &KbHandles, kb_name: &KbName) -> (u64, u64, String) {
    let cfg = state.config.read().await;
    let capture_cfg = cfg.server.capture.clone().unwrap_or_default();
    let dir = cfg
        .kb
        .get(kb_name)
        .map(|s| s.resolved_capture_dir().to_string())
        .unwrap_or_else(|| capture::DEFAULT_CAPTURE_DIR.to_string());
    (
        capture_cfg.max_file_bytes(),
        capture_cfg.max_request_bytes(),
        dir,
    )
}

/// Reject an oversized MULTI-FILE batch loud and early, before multipart
/// parsing even starts, rather than letting it die as generic multipart
/// noise once the per-file streaming cap trips partway through (U2
/// follow-up finding: two 6 MiB files each under a 10 MiB per-file cap died
/// with a 400 before per-file logic ever ran). Only fires when the client
/// sent a `Content-Length` that already exceeds the combined per-request
/// budget — every capture client (kb-cli, the SPA, the share sheet) sends
/// one, but a chunked/length-less request still falls through to the
/// existing streaming per-file 413 (unchanged) plus the outer
/// `DefaultBodyLimit` backstop (`router.rs`'s `capture_body_limit`).
pub(crate) fn reject_oversized_request(
    headers: &HeaderMap,
    max_request_bytes: u64,
) -> Option<Response<Body>> {
    let len: u64 = headers
        .get(header::CONTENT_LENGTH)
        .and_then(|v| v.to_str().ok())
        .and_then(|s| s.parse().ok())?;
    if len > max_request_bytes {
        Some(problem(
            StatusCode::PAYLOAD_TOO_LARGE,
            format!("combined upload exceeds max_request_bytes ({max_request_bytes})"),
        ))
    } else {
        None
    }
}

/// Shared handler core for both routes: check the combined-request budget,
/// drain the multipart body, then either capture each file (gated on
/// `ctx.ext_map`) or — no files, but a `url`/`text` share — write one
/// URL/text stub. `html_sanitize_default` is the route's own
/// opt-in-vs-share-target-untrusted-page default for HTML files
/// specifically (an explicit `sanitize` field always wins; Markdown ignores
/// it either way — U1 no-op).
///
/// **Validation is ALL-OR-NOTHING across the batch (U2 follow-up)**: every
/// file's extension is resolved against `ctx.ext_map` BEFORE the first
/// write, so a mixed batch (`good.md` + `bad.xyz`) 415s without leaving
/// `good.md` captured behind a pure-failure response (and without a
/// full-batch retry then duplicating it). Mid-batch IO failures (e.g. a
/// corpus going read-only → 409) may still leave earlier files written —
/// only the VALIDATION carries the all-or-nothing contract.
#[allow(clippy::too_many_arguments)]
async fn do_capture(
    state: &KbHandles,
    kb_name: &KbName,
    ctx: &KbContext,
    headers: &HeaderMap,
    multipart: &mut Multipart,
    default_from: String,
    html_sanitize_default: bool,
) -> Result<CaptureResponse, Response<Body>> {
    let (max_bytes, max_request_bytes, capture_dir) = capture_settings(state, kb_name).await;
    if let Some(resp) = reject_oversized_request(headers, max_request_bytes) {
        return Err(resp);
    }
    let form = drain_capture_form(multipart, max_bytes).await?;
    let from = form.from.clone().unwrap_or(default_from);

    if form.files.is_empty() {
        if form.url.is_none() && form.text.is_none() {
            return Err(problem(
                StatusCode::BAD_REQUEST,
                "capture requires at least one file, or a url/text share",
            ));
        }
        let out = capture::capture_url_stub(UrlStubInput {
            source_root: &ctx.source_path,
            capture_dir: &capture_dir,
            title: form.title.as_deref(),
            url: form.url.as_deref(),
            text: form.text.as_deref(),
            tags: &form.tags,
            from: &from,
        })
        .map_err(map_capture_error)?;
        return Ok(CaptureResponse {
            items: vec![CaptureItem {
                kb: kb_name.as_str().to_string(),
                id: out.id,
                source_relative: out.source_relative,
                title: out.title,
                url: form.url.clone(),
            }],
        });
    }

    // Pre-resolve EVERY file's pipeline before the write loop — see the
    // all-or-nothing validation note in the doc comment above. The write
    // loop below consumes these resolutions; it never re-consults the
    // ext_map.
    let mut resolved = Vec::with_capacity(form.files.len());
    for (fname, bytes) in &form.files {
        let name_for_ext = fname.as_deref().unwrap_or("");
        let Some(pipeline) = ctx.ext_map.pipeline(FsPath::new(name_for_ext)) else {
            return Err(problem(
                StatusCode::UNSUPPORTED_MEDIA_TYPE,
                format!(
                    "`{name_for_ext}` has no indexable extension for this kb (see \
                     [indexer.indexable_extensions] / [kb.*.indexable_extensions])"
                ),
            ));
        };
        let ext = FsPath::new(name_for_ext)
            .extension()
            .and_then(|s| s.to_str())
            .unwrap_or("");
        resolved.push((pipeline, ext, fname.as_deref(), bytes));
    }

    let mut items = Vec::with_capacity(resolved.len());
    for (pipeline, ext, fname, bytes) in resolved {
        let sanitize = form
            .sanitize
            .unwrap_or(html_sanitize_default && pipeline == Pipeline::Html);
        let out = capture::capture(CaptureInput {
            source_root: &ctx.source_path,
            capture_dir: &capture_dir,
            pipeline,
            ext,
            bytes,
            original_filename: fname,
            title: form.title.as_deref(),
            tags: &form.tags,
            from: &from,
            sanitize,
            url: form.url.as_deref(),
            stable_name: None,
            session_id: None,
            expires_at: None,
            category: None,
        })
        .map_err(map_capture_error)?;
        items.push(CaptureItem {
            kb: kb_name.as_str().to_string(),
            id: out.id,
            source_relative: out.source_relative,
            title: out.title,
            url: form.url.clone(),
        });
    }
    Ok(CaptureResponse { items })
}

/// `POST /api/kb/{kb}/capture` — see the module doc.
pub async fn create(
    State(state): State<Arc<KbHandles>>,
    Path(kb): Path<String>,
    headers: HeaderMap,
    mut multipart: Multipart,
) -> Response<Body> {
    let (kb_name, ctx) = match crate::routes::resolve_kb(&state, &kb) {
        Ok(v) => v,
        Err(resp) => return resp,
    };
    let default_from = from_default(&headers);
    match do_capture(
        &state,
        &kb_name,
        ctx,
        &headers,
        &mut multipart,
        default_from,
        false,
    )
    .await
    {
        Ok(resp) => (StatusCode::CREATED, Json(resp)).into_response(),
        Err(resp) => resp,
    }
}

/// Resolve the Web Share Target destination kb: `[server.capture].default_kb`
/// when it names a configured kb, else the first configured kb (`state.kbs`
/// is a `BTreeMap` — lexicographically-first, deterministic; see
/// `CaptureSection::default_kb`'s doc).
async fn share_target_kb(state: &KbHandles) -> Option<KbName> {
    let cfg = state.config.read().await;
    if let Some(name) = cfg
        .server
        .capture
        .as_ref()
        .and_then(|c| c.default_kb.as_deref())
    {
        if let Ok(kb) = KbName::new(name) {
            if state.kbs.contains_key(&kb) {
                return Some(kb);
            }
        }
    }
    state.kbs.keys().next().cloned()
}

/// `encodeURIComponent`-shaped percent-encoder for the `/?captured=`
/// redirect value (`<kb>:<source_relative>` — the SPA reads it via
/// `URLSearchParams`/`decodeURIComponent`, so `:` and `/` must be escaped
/// like any other query-value byte, not left bare). Cheap to inline vs.
/// pulling in `percent-encoding` for one call site — same rationale as
/// `routes::docs::url_q`.
fn percent_encode_component(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.as_bytes() {
        let safe = b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b'~');
        if safe {
            out.push(*b as char);
        } else {
            out.push_str(&format!("%{:02X}", b));
        }
    }
    out
}

/// `POST /capture` — the Web Share Target action. See the module doc for
/// the TRAP note on why `router.rs` must layer auth onto this route
/// explicitly.
pub async fn share_target(
    State(state): State<Arc<KbHandles>>,
    headers: HeaderMap,
    mut multipart: Multipart,
) -> Response<Body> {
    let Some(kb_name) = share_target_kb(&state).await else {
        return error_to_problem_json(&kb_core::Error::NotFound(
            "no kb configured to capture into".to_string(),
        ));
    };
    let Some(ctx) = state.kbs.get(&kb_name) else {
        return error_to_problem_json(&kb_core::Error::NotFound(format!("kb {kb_name}")));
    };
    match do_capture(
        &state,
        &kb_name,
        ctx,
        &headers,
        &mut multipart,
        "share".to_string(),
        true,
    )
    .await
    {
        Ok(resp) => {
            let location = match resp.items.first() {
                Some(item) => format!(
                    "/?captured={}",
                    percent_encode_component(&format!("{}:{}", item.kb, item.source_relative))
                ),
                None => "/".to_string(),
            };
            Response::builder()
                .status(StatusCode::SEE_OTHER)
                .header(header::LOCATION, location)
                .body(Body::empty())
                .unwrap_or_else(|_| {
                    error_to_problem_json(&kb_core::Error::Storage(
                        "response build failed".to_string(),
                    ))
                })
        }
        Err(resp) => resp,
    }
}
