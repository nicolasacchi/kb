//! V3.4-C1 — canvas-set persistence (SPA working-set canvas durability).
//!
//! The SPA's Code Bubbles-style, read-only fragment canvas persists per
//! review/session. The server owns durability ONLY — layout/geometry is an
//! opaque client JSON payload. Cap: **256 KiB** (`MAX_PAYLOAD_BYTES`);
//! overflow is a hard `413` with the byte count — never silent truncation.
//!
//! # Wire routes
//!
//! - `GET /api/canvas?repo=` — ordinary `auth_bearer` (list, no payload body)
//! - `GET /api/canvas/{id}` — ordinary `auth_bearer` (full row)
//! - `POST /api/canvas` — **loopback-only** (create)
//! - `PUT /api/canvas/{id}` — **loopback-only** (update payload / rename)
//! - `DELETE /api/canvas/{id}` — **loopback-only**
//!
//! Mutation gate copies the V3.R1 review-mutation precedent in
//! `router.rs` (`transcripts_api` + `loopback_only`) — not a new gate.
//! Timestamps are server-derived (`SystemTime`); client clocks are never
//! trusted.

use crate::routes::{find_repo, find_repo_by_id, ApiError};
use crate::state::SharedState;
use crate::store::{StoreBlocking, StoreError};
use axum::extract::{Path as AxumPath, Query, State};
use axum::http::{header, StatusCode};
use axum::response::IntoResponse;
use axum::Json;
use serde::{Deserialize, Serialize};
use std::time::{SystemTime, UNIX_EPOCH};

pub const SCHEMA: &str = "canvas/1";

/// Hard cap on the opaque payload JSON (UTF-8 bytes of the serialized form).
pub const MAX_PAYLOAD_BYTES: usize = 256 * 1024;

fn now_unix() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/// Serialize `payload` and enforce the 256 KiB cap. Returns the JSON text
/// on success; `413` with the actual byte count on overflow.
/// Raw-body allowance: the payload cap plus slack for the envelope
/// fields (repo/name/review_id + JSON syntax). Anything over this is
/// rejected BEFORE deserialization; `encode_payload` then enforces the
/// exact per-field cap with an accurate byte count.
const ENVELOPE_SLACK: usize = 8 * 1024;

fn reject_oversize_raw(n: usize) -> Result<(), ApiError> {
    if n > MAX_PAYLOAD_BYTES + ENVELOPE_SLACK {
        return Err(ApiError::new(
            StatusCode::PAYLOAD_TOO_LARGE,
            format!(
                "canvas request body is {n} bytes; hard cap is {MAX_PAYLOAD_BYTES} bytes \
                 (256 KiB) of payload plus {ENVELOPE_SLACK} bytes of envelope — \
                 the canvas payload must stay under the cap"
            ),
        ));
    }
    Ok(())
}

pub fn encode_payload(payload: &serde_json::Value) -> Result<String, ApiError> {
    let text = serde_json::to_string(payload)
        .map_err(|e| ApiError::bad_request(format!("payload is not serializable JSON: {e}")))?;
    let n = text.len();
    if n > MAX_PAYLOAD_BYTES {
        return Err(ApiError::new(
            StatusCode::PAYLOAD_TOO_LARGE,
            format!(
                "canvas payload is {n} bytes; hard cap is {MAX_PAYLOAD_BYTES} bytes (256 KiB) — \
                 refused, not truncated"
            ),
        ));
    }
    Ok(text)
}

fn map_store(e: StoreError) -> ApiError {
    match e {
        StoreError::NameConflict(name) => ApiError::new(
            StatusCode::CONFLICT,
            format!("a canvas set named {name:?} already exists in this repo"),
        ),
        other => ApiError::from(other),
    }
}

// --- list ----------------------------------------------------------------

#[derive(Debug, Deserialize)]
pub struct ListCanvasParams {
    pub repo: String,
}

#[derive(Debug, Serialize)]
pub struct CanvasSummary {
    pub id: i64,
    pub name: String,
    pub review_id: Option<i64>,
    pub updated_unix: i64,
    pub payload_bytes: i64,
}

#[derive(Debug, Serialize)]
pub struct CanvasListOut {
    pub schema: &'static str,
    pub repo: String,
    pub items: Vec<CanvasSummary>,
}

/// `GET /api/canvas?repo=` — list (no payload body).
pub async fn list_canvas(
    State(state): State<SharedState>,
    Query(params): Query<ListCanvasParams>,
) -> Result<impl IntoResponse, ApiError> {
    let (_repo, repo_id) = find_repo(&state, &params.repo)?;
    let repo_name = params.repo.clone();
    // 2026-08-31 incident (store.rs module doc): the list + JSON compose
    // has no `.await` in it — one closure on the blocking pool.
    let out = state
        .store
        .run_blocking(move |store| {
            let rows = store.list_canvas_sets(repo_id).map_err(map_store)?;
            let items = rows
                .into_iter()
                .map(|r| CanvasSummary {
                    id: r.id,
                    name: r.name,
                    review_id: r.review_id,
                    updated_unix: r.updated_unix,
                    payload_bytes: r.payload_bytes,
                })
                .collect();
            Ok::<_, ApiError>(CanvasListOut {
                schema: SCHEMA,
                repo: repo_name,
                items,
            })
        })
        .await?;
    Ok(([(header::CACHE_CONTROL, "no-store")], Json(out)))
}

// --- get -----------------------------------------------------------------

#[derive(Debug, Serialize)]
pub struct CanvasView {
    pub schema: &'static str,
    pub id: i64,
    pub repo: String,
    pub name: String,
    pub review_id: Option<i64>,
    pub payload: serde_json::Value,
    pub created_unix: i64,
    pub updated_unix: i64,
}

/// `GET /api/canvas/{id}` — full row incl. payload.
pub async fn get_canvas(
    State(state): State<SharedState>,
    AxumPath(id): AxumPath<i64>,
) -> Result<impl IntoResponse, ApiError> {
    // 2026-08-31 incident (store.rs module doc): the store read runs on
    // the blocking pool; `find_repo_by_id` below is a cheap in-memory
    // lookup over `state.repos`, not a store call, so it stays outside.
    let row = state
        .store
        .run_blocking(move |store| store.get_canvas_set(id).map_err(map_store))
        .await?
        .ok_or_else(|| ApiError::not_found(format!("canvas set {id}")))?;
    let repo = find_repo_by_id(&state, row.repo_id)?;
    let payload: serde_json::Value = serde_json::from_str(&row.payload).unwrap_or_else(|_| {
        // Stored as opaque text; if a row is somehow non-JSON, surface raw.
        serde_json::Value::String(row.payload.clone())
    });
    Ok((
        [(header::CACHE_CONTROL, "no-store")],
        Json(CanvasView {
            schema: SCHEMA,
            id: row.id,
            repo: repo.name.clone(),
            name: row.name,
            review_id: row.review_id,
            payload,
            created_unix: row.created_unix,
            updated_unix: row.updated_unix,
        }),
    ))
}

// --- create --------------------------------------------------------------

#[derive(Debug, Deserialize)]
pub struct CreateCanvasBody {
    pub repo: String,
    pub name: String,
    #[serde(default)]
    pub review_id: Option<i64>,
    pub payload: serde_json::Value,
}

/// `POST /api/canvas` — create; `409` on name collision; `413` on payload
/// over the 256 KiB cap. LOOPBACK-ONLY (router gate).
pub async fn create_canvas(
    State(state): State<SharedState>,
    raw: String,
) -> Result<impl IntoResponse, ApiError> {
    // Cap on RAW bytes BEFORE any JSON parse (the parse cost must be
    // bounded by the cap, not by axum's 2 MiB default body limit; same
    // posture as kb-server's atlas_field::put_field). ENVELOPE_SLACK
    // covers the non-payload fields (repo/name/review_id + JSON syntax).
    reject_oversize_raw(raw.len())?;
    let body: CreateCanvasBody = serde_json::from_str(&raw)
        .map_err(|e| ApiError::bad_request(format!("invalid canvas body: {e}")))?;
    if body.name.trim().is_empty() {
        return Err(ApiError::bad_request("name must be non-empty"));
    }
    let (_repo, repo_id) = find_repo(&state, &body.repo)?;
    let payload_text = encode_payload(&body.payload)?;
    let now = now_unix();
    let name = body.name.clone();
    let review_id = body.review_id;
    // 2026-08-31 incident (store.rs module doc): the two sequential store
    // calls (create then re-fetch) run as one closure on the blocking pool.
    let row = state
        .store
        .run_blocking(move |store| {
            let id = store
                .create_canvas_set(repo_id, &name, review_id, &payload_text, now)
                .map_err(map_store)?;
            store.get_canvas_set(id).map_err(map_store)?.ok_or_else(|| {
                ApiError::new(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    format!("canvas set {id} vanished immediately after create"),
                )
            })
        })
        .await?;
    let repo = find_repo_by_id(&state, row.repo_id)?;
    Ok((
        StatusCode::CREATED,
        [(header::CACHE_CONTROL, "no-store")],
        Json(CanvasView {
            schema: SCHEMA,
            id: row.id,
            repo: repo.name.clone(),
            name: row.name,
            review_id: row.review_id,
            payload: body.payload,
            created_unix: row.created_unix,
            updated_unix: row.updated_unix,
        }),
    ))
}

// --- update --------------------------------------------------------------

#[derive(Debug, Deserialize)]
pub struct UpdateCanvasBody {
    pub payload: serde_json::Value,
    #[serde(default)]
    pub name: Option<String>,
}

/// `PUT /api/canvas/{id}` — replace payload (optional rename); bumps
/// `updated_unix`. LOOPBACK-ONLY.
pub async fn update_canvas(
    State(state): State<SharedState>,
    AxumPath(id): AxumPath<i64>,
    raw: String,
) -> Result<impl IntoResponse, ApiError> {
    // Same raw-bytes-before-parse cap as create_canvas.
    reject_oversize_raw(raw.len())?;
    let body: UpdateCanvasBody = serde_json::from_str(&raw)
        .map_err(|e| ApiError::bad_request(format!("invalid canvas body: {e}")))?;
    if let Some(ref n) = body.name {
        if n.trim().is_empty() {
            return Err(ApiError::bad_request(
                "name must be non-empty when provided",
            ));
        }
    }
    let payload_text = encode_payload(&body.payload)?;
    let now = now_unix();
    let name = body.name.clone();
    // 2026-08-31 incident (store.rs module doc): existence check, update,
    // and re-fetch are three sequential store calls with no async work
    // between them — one closure, one hop to the blocking pool.
    let row = state
        .store
        .run_blocking(move |store| {
            // Existence check first so a missing id is 404 (not a silent
            // no-op update); the store update also returns false if the
            // row vanished.
            store
                .get_canvas_set(id)
                .map_err(map_store)?
                .ok_or_else(|| ApiError::not_found(format!("canvas set {id}")))?;
            let ok = store
                .update_canvas_set(id, &payload_text, name.as_deref(), now)
                .map_err(map_store)?;
            if !ok {
                return Err(ApiError::not_found(format!("canvas set {id}")));
            }
            store
                .get_canvas_set(id)
                .map_err(map_store)?
                .ok_or_else(|| ApiError::not_found(format!("canvas set {id}")))
        })
        .await?;
    let repo = find_repo_by_id(&state, row.repo_id)?;
    Ok((
        [(header::CACHE_CONTROL, "no-store")],
        Json(CanvasView {
            schema: SCHEMA,
            id: row.id,
            repo: repo.name.clone(),
            name: row.name,
            review_id: row.review_id,
            payload: body.payload,
            created_unix: row.created_unix,
            updated_unix: row.updated_unix,
        }),
    ))
}

// --- delete --------------------------------------------------------------

/// `DELETE /api/canvas/{id}` — LOOPBACK-ONLY.
pub async fn delete_canvas(
    State(state): State<SharedState>,
    AxumPath(id): AxumPath<i64>,
) -> Result<impl IntoResponse, ApiError> {
    // 2026-08-31 incident (store.rs module doc): single store call, still
    // wrapped so it can never park this async worker on the mutex wait.
    let ok = state
        .store
        .run_blocking(move |store| store.delete_canvas_set(id).map_err(map_store))
        .await?;
    if !ok {
        return Err(ApiError::not_found(format!("canvas set {id}")));
    }
    Ok((
        StatusCode::NO_CONTENT,
        [(header::CACHE_CONTROL, "no-store")],
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn payload_cap_exact_256kib_passes() {
        // Build a JSON string whose serialized form is exactly MAX_PAYLOAD_BYTES.
        // {"d":"<pad>"} — overhead of `{"d":""}` is 8 bytes.
        let overhead = serde_json::to_string(&serde_json::json!({"d": ""}))
            .unwrap()
            .len();
        assert_eq!(overhead, 8);
        let pad_len = MAX_PAYLOAD_BYTES - overhead;
        let pad = "x".repeat(pad_len);
        let v = serde_json::json!({"d": pad});
        let text = encode_payload(&v).expect("exact 256 KiB must pass");
        assert_eq!(text.len(), MAX_PAYLOAD_BYTES);
    }

    #[test]
    fn payload_cap_one_byte_over_is_413() {
        let overhead = serde_json::to_string(&serde_json::json!({"d": ""}))
            .unwrap()
            .len();
        let pad = "x".repeat(MAX_PAYLOAD_BYTES - overhead + 1);
        let v = serde_json::json!({"d": pad});
        let err = encode_payload(&v).expect_err("+1 byte must 413");
        assert!(
            err.message().contains(&(MAX_PAYLOAD_BYTES + 1).to_string())
                || err.message().contains("bytes")
        );
        // Surface as 413 via IntoResponse status — check the constructed status.
        let resp = err.into_response();
        assert_eq!(resp.status(), StatusCode::PAYLOAD_TOO_LARGE);
    }

    #[test]
    fn payload_cap_message_includes_byte_count() {
        let overhead = 8;
        let n = MAX_PAYLOAD_BYTES + 1;
        let pad = "x".repeat(n - overhead);
        let v = serde_json::json!({"d": pad});
        let err = encode_payload(&v).unwrap_err();
        assert!(
            err.message().contains(&n.to_string()),
            "413 body must name the actual byte count, got: {}",
            err.message()
        );
        assert!(err.message().contains(&MAX_PAYLOAD_BYTES.to_string()));
    }
}
