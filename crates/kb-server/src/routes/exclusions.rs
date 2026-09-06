//! X3 (v0.24) — per-file exclusion API.
//!
//! - `GET    /api/kb/{kb}/exclusions`        — list current exclusions
//! - `POST   /api/kb/{kb}/exclusions`        — exclude (body `{path, note?}`)
//! - `DELETE /api/kb/{kb}/exclusions/{path}` — re-include (`path` is one
//!   percent-encoded segment; `/` inside it travels as `%2F`)
//!
//! Thin HTTP shell over `kb_core::exclusions::{exclude_file, include_file}`
//! (X2): exclude = durable sqlite row + live gate entry + a KeepUserData
//! cascade through the ingest sink (index row goes, `.review` sidecar +
//! reading history survive); include = row/gate removal + a forced reindex
//! nudge. The index-generation bump and the `artifact.removed` /
//! `artifact.indexed` events ride that ingest pipeline (invariant #15 is
//! satisfied downstream, not here). These routes ADD the intent-level SSE
//! kinds — `artifact.excluded` / `artifact.included` — which fire only on
//! an actual state change, so replaying a POST/DELETE is idempotent on the
//! wire (the response's `newly_excluded`/`was_excluded` flag says which).

use crate::middleware::error_to_problem_json;
use crate::state::KbHandles;
use axum::{
    body::Body,
    extract::{Path, State},
    http::Response,
    response::IntoResponse,
    Json,
};
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::sync::Arc;

/// One exclusion row, enriched for the SPA pane / CLI table.
#[cfg_attr(feature = "ts-export", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Serialize)]
pub struct ExclusionEntry {
    /// Source-relative, forward-slash path (normalised at write time).
    pub path: String,
    /// Unix seconds when the exclusion was recorded.
    pub excluded_at: i64,
    /// Optional operator note stored with the exclusion.
    pub note: Option<String>,
    /// The deterministic artifact id this path derives to
    /// (`ArtifactId::from_path` — invariant #27: ids are source-relative).
    pub artifact_id: String,
    /// Whether the excluded file is still present on disk under the source
    /// root (false = it was deleted while excluded; re-including it would
    /// only drop the row, nothing reindexes).
    pub present_on_disk: bool,
}

#[cfg_attr(feature = "ts-export", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Deserialize)]
pub struct ExcludeBody {
    /// Source-relative path to exclude (normalised server-side).
    pub path: String,
    /// Optional operator note ("why is this excluded").
    pub note: Option<String>,
}

#[cfg_attr(feature = "ts-export", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Serialize)]
pub struct ExcludeResponse {
    pub kb: String,
    pub path: String,
    pub artifact_id: String,
    /// `false` when the path was already excluded (idempotent re-POST —
    /// the delete is still re-pushed to self-heal a half-applied attempt).
    pub newly_excluded: bool,
}

#[cfg_attr(feature = "ts-export", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Serialize)]
pub struct IncludeResponse {
    pub kb: String,
    pub path: String,
    pub artifact_id: String,
    /// `false` when the path wasn't excluded (nothing changed).
    pub was_excluded: bool,
}

fn artifact_id_for(rel: &str) -> String {
    kb_core::ids::ArtifactId::from_path(rel)
        .as_str()
        .to_string()
}

pub async fn list(State(state): State<Arc<KbHandles>>, Path(kb): Path<String>) -> Response<Body> {
    let (_kb_name, ctx) = match crate::routes::resolve_kb(&state, &kb) {
        Ok(v) => v,
        Err(resp) => return resp,
    };
    let rows = match ctx.storage.list_exclusions().await {
        Ok(r) => r,
        Err(e) => return error_to_problem_json(&e),
    };
    let entries: Vec<ExclusionEntry> = rows
        .into_iter()
        .map(|r| ExclusionEntry {
            artifact_id: artifact_id_for(&r.path),
            present_on_disk: ctx.source_path.join(&r.path).exists(),
            path: r.path,
            excluded_at: r.excluded_at_unix,
            note: r.note,
        })
        .collect();
    Json(entries).into_response()
}

pub async fn create(
    State(state): State<Arc<KbHandles>>,
    Path(kb): Path<String>,
    Json(body): Json<ExcludeBody>,
) -> Response<Body> {
    let (kb_name, ctx) = match crate::routes::resolve_kb(&state, &kb) {
        Ok(v) => v,
        Err(resp) => return resp,
    };
    // Normalise up front so the response + event carry the exact stored
    // form; exclude_file re-normalises + validates (empty / `..` → 400).
    let rel = kb_core::exclusions::normalize_rel(&body.path);
    let newly_excluded = match kb_core::exclusions::exclude_file(
        &ctx.storage,
        &ctx.ingest,
        &ctx.source_path,
        &rel,
        body.note,
    )
    .await
    {
        Ok(added) => added,
        Err(e) => return error_to_problem_json(&e),
    };
    let artifact_id = artifact_id_for(&rel);
    if newly_excluded {
        ctx.bus.emit(
            "artifact.excluded",
            json!({"kb": kb_name.as_str(), "path": rel, "artifact_id": artifact_id}),
        );
    }
    Json(ExcludeResponse {
        kb: kb_name.as_str().to_string(),
        path: rel,
        artifact_id,
        newly_excluded,
    })
    .into_response()
}

pub async fn remove(
    State(state): State<Arc<KbHandles>>,
    Path((kb, path)): Path<(String, String)>,
) -> Response<Body> {
    let (kb_name, ctx) = match crate::routes::resolve_kb(&state, &kb) {
        Ok(v) => v,
        Err(resp) => return resp,
    };
    let rel = kb_core::exclusions::normalize_rel(&path);
    let was_excluded =
        match kb_core::exclusions::include_file(&ctx.storage, &ctx.ingest, &ctx.source_path, &rel)
            .await
        {
            Ok(removed) => removed,
            Err(e) => return error_to_problem_json(&e),
        };
    let artifact_id = artifact_id_for(&rel);
    if was_excluded {
        ctx.bus.emit(
            "artifact.included",
            json!({"kb": kb_name.as_str(), "path": rel, "artifact_id": artifact_id}),
        );
    }
    Json(IncludeResponse {
        kb: kb_name.as_str().to_string(),
        path: rel,
        artifact_id,
        was_excluded,
    })
    .into_response()
}
