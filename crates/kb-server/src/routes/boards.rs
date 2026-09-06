//! `boards` — Boards v1 (W2.4): a JSON Canvas `.canvas` sidecar per
//! reading list, geometry ONLY (standing rule 2 — no third store). The
//! sidecar lives IN THE CORPUS at `<source_root>/boards/<list_id>.canvas`,
//! written/read the way the notes `create`/`update` handlers write a
//! note (`crate::routes::notes`: folder confinement → `create_dir_all` →
//! `kb_core::fsx::write_atomic`) — but this route deliberately skips
//! `nudge_indexer`. `.canvas` has no entry in any kb's `ExtensionMap`
//! (X1 — exactly two parse pipelines, html/md), so the file is invisible
//! to the indexer BY DESIGN: no lance row, no watcher reindex, no
//! `index_generation` bump (invariants #15/#25c both apply — a board
//! mutation must never look like a row-set/list mutation). This route
//! pair is the ONLY code that ever reads or writes the file.
//!
//! Routes:
//!
//! ```text
//! GET /api/kb/{kb}/boards/{list_id}/canvas   → raw JSON Canvas doc
//!                                               (absent file → the
//!                                               empty default)
//! PUT /api/kb/{kb}/boards/{list_id}/canvas   → replace it wholesale
//! ```
//!
//! Both directions are "store what parses" — the daemon does NOT
//! deep-validate the JSON Canvas 1.0 spec (that's the interop contract
//! itself, and the SPA/CLI's problem); it only confirms the body is
//! valid JSON shaped like `{nodes: [...], edges: [...]}` (arrays IF
//! present — both keys are optional per spec) before writing the
//! caller's bytes to disk VERBATIM (no serde round-trip through the
//! daemon, so unknown/future JSON Canvas fields — and formatting —
//! survive untouched; that's `web/src/lib/canvas.ts`'s job on read).
//! `PUT` is capped at [`MAX_CANVAS_BYTES`] (256 KiB) — a board is a few
//! dozen node/edge objects, not a bulk-import target.
//!
//! SSE: `board.updated` carries `{kb, list_id}` (the whole doc is
//! replaced on every write, so there's no per-field diff to report);
//! the SPA bridge invalidates `["board", kb, list_id]`.
//!
//! No wire struct, no `#[derive(TS)]` / ts-export: the response body
//! IS the JSON Canvas document, an open-ended interop format ("the
//! format IS the interop" — the file's own design note), so there is no
//! meaningful Rust shape to bind against; a ts-rs struct here would just
//! be a redundant `Record<string, unknown>` that drifts from the real
//! contract instead of documenting it.
//!
//! 404 when the list itself doesn't exist (existence check reuses the
//! same `ctx.storage.list_get` read `routes::lists` already does); a
//! read-only corpus (prod's `:ro` bind-mount) surfaces a write failure
//! as 409, mirroring `routes::capture`'s `map_capture_error`.

use crate::middleware::error_to_problem_json;
use crate::state::KbHandles;
use axum::{
    body::Body,
    extract::{Path, State},
    http::{header, HeaderValue, Response, StatusCode},
    response::IntoResponse,
};
use serde_json::Value;
use std::path::{Path as FsPath, PathBuf};
use std::sync::Arc;

/// The empty JSON Canvas document — served whenever no sidecar has been
/// written yet (a fresh list has no board). Byte-identical every call
/// (no HashMap iteration, no clock) — matters for anyone diffing/
/// snapshotting the corpus.
const DEFAULT_CANVAS: &str = r#"{"nodes":[],"edges":[]}"#;

/// `PUT` body cap — a board is a UI board, not a bulk-import target.
const MAX_CANVAS_BYTES: usize = 256 * 1024;

/// Confirm `bytes` parses as JSON shaped like a JSON Canvas document: a
/// top-level object whose `nodes`/`edges` keys, if present, are arrays.
/// Deliberately shallow — nothing below the top level (node/edge shape)
/// is inspected. "Store what parses": the interop format's own schema
/// is the validation, not a duplicate Rust model of JSON Canvas 1.0.
fn validate_canvas_bytes(bytes: &[u8]) -> Result<(), kb_core::Error> {
    let v: Value = serde_json::from_slice(bytes)
        .map_err(|e| kb_core::Error::BadRequest(format!("canvas is not valid JSON: {e}")))?;
    let obj = v
        .as_object()
        .ok_or_else(|| kb_core::Error::BadRequest("canvas must be a JSON object".into()))?;
    for key in ["nodes", "edges"] {
        if let Some(val) = obj.get(key) {
            if !val.is_array() {
                return Err(kb_core::Error::BadRequest(format!(
                    "canvas {key:?} must be an array"
                )));
            }
        }
    }
    Ok(())
}

/// Sidecar path for one list's board. Confinement is the CALLER's job —
/// every call site validates `list_id` via [`crate::routes::is_safe_id`]
/// first (rejects `..` and any path separator, among other things), so
/// this function does no re-checking of its own.
fn board_path(source_root: &FsPath, list_id: &str) -> PathBuf {
    source_root.join("boards").join(format!("{list_id}.canvas"))
}

/// Wrap `bytes` as a 200 (or the given status) `application/json`
/// response, verbatim — no serde re-encoding, so whatever was on disk
/// (or whatever the caller just PUT) comes back byte-for-byte.
fn json_response(status: StatusCode, bytes: Vec<u8>) -> Response<Body> {
    let mut resp = (status, bytes).into_response();
    resp.headers_mut().insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("application/json"),
    );
    resp
}

/// A read-only corpus (prod's `:ro` bind-mount) surfaces as a plain
/// `Error::Io(PermissionDenied)`; map it to 409 — same treatment as
/// `routes::capture`'s `map_capture_error` — the request is well-formed,
/// the destination just can't be written to right now.
fn map_write_error(e: kb_core::Error) -> Response<Body> {
    if let kb_core::Error::Io(io_err) = &e {
        if io_err.kind() == std::io::ErrorKind::PermissionDenied {
            return error_to_problem_json(&kb_core::Error::Conflict(
                "board destination is read-only".to_string(),
            ));
        }
    }
    error_to_problem_json(&e)
}

/// `GET /api/kb/{kb}/boards/{list_id}/canvas` — the raw JSON Canvas doc,
/// or [`DEFAULT_CANVAS`] when no sidecar has been written yet. 404 when
/// the list itself doesn't exist.
pub async fn get_canvas(
    State(state): State<Arc<KbHandles>>,
    Path((kb, list_id)): Path<(String, String)>,
) -> Response<Body> {
    let (kb_name, ctx) = match crate::routes::resolve_kb(&state, &kb) {
        Ok(v) => v,
        Err(resp) => return resp,
    };
    if !crate::routes::is_safe_id(&list_id) {
        return error_to_problem_json(&kb_core::Error::BadRequest("invalid list id".into()));
    }
    match ctx.storage.list_get(list_id.clone()).await {
        Ok(Some(_)) => {}
        Ok(None) => {
            return error_to_problem_json(&kb_core::Error::NotFound(format!(
                "list {list_id} in kb {kb_name}"
            )))
        }
        Err(e) => return error_to_problem_json(&e),
    }
    let path = board_path(&ctx.source_path, &list_id);
    let bytes = match std::fs::read(&path) {
        Ok(b) => b,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return json_response(StatusCode::OK, DEFAULT_CANVAS.as_bytes().to_vec());
        }
        Err(e) => {
            return error_to_problem_json(&kb_core::Error::Storage(format!(
                "read {}: {e}",
                path.display()
            )))
        }
    };
    if let Err(e) = validate_canvas_bytes(&bytes) {
        return error_to_problem_json(&kb_core::Error::Storage(format!(
            "board canvas for list {list_id} in kb {kb_name} is corrupt: {e}"
        )));
    }
    json_response(StatusCode::OK, bytes)
}

/// `PUT /api/kb/{kb}/boards/{list_id}/canvas` — replace the sidecar
/// wholesale. The body is stored VERBATIM (no serde re-serialization)
/// once it passes [`validate_canvas_bytes`] and the size cap. No index
/// interaction whatsoever — see the module doc.
pub async fn put_canvas(
    State(state): State<Arc<KbHandles>>,
    Path((kb, list_id)): Path<(String, String)>,
    body: String,
) -> Response<Body> {
    let (kb_name, ctx) = match crate::routes::resolve_kb(&state, &kb) {
        Ok(v) => v,
        Err(resp) => return resp,
    };
    if !crate::routes::is_safe_id(&list_id) {
        return error_to_problem_json(&kb_core::Error::BadRequest("invalid list id".into()));
    }
    if body.len() > MAX_CANVAS_BYTES {
        return error_to_problem_json(&kb_core::Error::BadRequest(format!(
            "canvas body too large: {} bytes (cap {MAX_CANVAS_BYTES})",
            body.len()
        )));
    }
    match ctx.storage.list_get(list_id.clone()).await {
        Ok(Some(_)) => {}
        Ok(None) => {
            return error_to_problem_json(&kb_core::Error::NotFound(format!(
                "list {list_id} in kb {kb_name}"
            )))
        }
        Err(e) => return error_to_problem_json(&e),
    }
    if let Err(e) = validate_canvas_bytes(body.as_bytes()) {
        return error_to_problem_json(&e);
    }
    let path = board_path(&ctx.source_path, &list_id);
    if let Some(parent) = path.parent() {
        if let Err(e) = std::fs::create_dir_all(parent) {
            return map_write_error(kb_core::Error::Io(e));
        }
    }
    if let Err(e) = kb_core::fsx::write_atomic(&path, body.as_bytes()) {
        return map_write_error(e);
    }
    ctx.bus.emit(
        "board.updated",
        serde_json::json!({ "kb": kb_name.as_str(), "list_id": list_id }),
    );
    json_response(StatusCode::OK, body.into_bytes())
}

#[cfg(test)]
mod tests {
    use super::*;

    // invariant:25 — a board write must never look like a list-row or
    // index mutation; these pin the pure helpers the routes above build
    // on (path confinement input, size cap, shallow JSON-shape check).

    #[test]
    fn validate_canvas_bytes_accepts_the_default() {
        assert!(validate_canvas_bytes(DEFAULT_CANVAS.as_bytes()).is_ok());
    }

    #[test]
    fn validate_canvas_bytes_accepts_absent_nodes_edges() {
        assert!(validate_canvas_bytes(b"{}").is_ok());
    }

    #[test]
    fn validate_canvas_bytes_rejects_non_json() {
        assert!(validate_canvas_bytes(b"not json").is_err());
    }

    #[test]
    fn validate_canvas_bytes_rejects_non_object() {
        assert!(validate_canvas_bytes(b"[1,2,3]").is_err());
    }

    #[test]
    fn validate_canvas_bytes_rejects_non_array_nodes() {
        assert!(validate_canvas_bytes(br#"{"nodes":"oops","edges":[]}"#).is_err());
    }

    #[test]
    fn validate_canvas_bytes_rejects_non_array_edges() {
        assert!(validate_canvas_bytes(br#"{"nodes":[],"edges":{}}"#).is_err());
    }

    #[test]
    fn validate_canvas_bytes_preserves_unknown_top_level_shape() {
        // The daemon only checks nodes/edges — an extra top-level key
        // (a future JSON Canvas field) must not be rejected.
        assert!(validate_canvas_bytes(br#"{"nodes":[],"edges":[],"future":true}"#).is_ok());
    }

    #[test]
    fn board_path_confines_under_a_boards_dir() {
        let root = FsPath::new("/tmp/kb-corpus");
        let p = board_path(root, "l_0123456789ab");
        assert_eq!(p, root.join("boards").join("l_0123456789ab.canvas"));
    }

    #[test]
    fn is_safe_id_rejects_traversal_and_separators() {
        // The shared confinement predicate every call site gates on
        // before ever building a `board_path` — reused rather than
        // reinvented (routes::mod's doc: alnum/-/_/. only, no `..`).
        assert!(crate::routes::is_safe_id("l_0123456789ab"));
        assert!(!crate::routes::is_safe_id(".."));
        assert!(!crate::routes::is_safe_id("../../etc/passwd"));
        assert!(!crate::routes::is_safe_id("l_abc/def"));
        assert!(!crate::routes::is_safe_id(""));
    }

    #[test]
    fn max_canvas_bytes_is_256_kib() {
        assert_eq!(MAX_CANVAS_BYTES, 256 * 1024);
    }
}
