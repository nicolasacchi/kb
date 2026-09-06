//! `GET /api/anchors/stale` — fleet-wide cold load for the SPA stale-
//! anchors dashboard.
//!
//! The dashboard is session-scoped by default: it accumulates rows from
//! the live SSE firehose (`comment.anchor_stale` adds, `comment.anchor_resolved`
//! removes). When the operator hits the page right after a reload, the
//! list is empty until something triggers an event. This endpoint
//! closes that gap by reading every kb's `.anchors-stale.json` sidecar
//! (the indexer's persisted stale set — see `kb_core::anchors`) and
//! returning the union, so the SPA can seed its session state with what
//! the daemon already knows.
//!
//! Shape:
//! ```json
//! { "anchors": [
//!     {"kb": "canon", "artifact_id": "abc12...", "comment_id": "c_one"},
//!     {"kb": "work",  "artifact_id": "def34...", "comment_id": "c_two"}
//!   ]
//! }
//! ```
//!
//! Per-entry metadata that the live SSE event carries (`anchor_kind`,
//! `fuzzy_score`) is NOT in the sidecar — the sidecar only persists the
//! `(artifact, comment)` pair. Live events stay the source of truth for
//! those fields; the cold-load is "things that are *probably* still
//! stale right now". A subsequent reindex updates both.

use crate::middleware::error_to_problem_json;
use crate::state::KbHandles;
use axum::{
    body::Body,
    extract::{Path, State},
    http::{Response, StatusCode},
    response::IntoResponse,
    Json,
};
use serde::Serialize;
use std::sync::Arc;

#[cfg_attr(
    feature = "ts-export",
    derive(ts_rs::TS),
    ts(export, rename = "StaleAnchorRow")
)]
#[derive(Debug, Serialize)]
pub struct StaleAnchorOut {
    pub kb: String,
    pub artifact_id: String,
    pub comment_id: String,
    /// v3 sidecar metadata — anchor scope name (file/chapter/section/
    /// selection) captured at the most recent stale transition.
    /// Defaults to "stale" for entries loaded from a pre-v3 sidecar.
    pub anchor_kind: String,
    /// v3 sidecar metadata — fuzzy resolver's best-tried score on the
    /// stale transition. 0 for unit-variant Stale outcomes (#4); the
    /// schema is ready to round-trip a richer value if Resolution
    /// grows a score on Stale.
    pub fuzzy_score: f32,
    /// Track U — source-root-relative path of the artifact, used to
    /// build the `/a/<kb>/<source_relative>` permalink. None when the
    /// artifact is no longer in lance (sidecar outlived the file).
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts-export", ts(optional))]
    pub source_relative: Option<String>,
}

#[cfg_attr(feature = "ts-export", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Serialize)]
pub struct StaleAnchorsResponse {
    pub anchors: Vec<StaleAnchorOut>,
}

// === Anchor corkboard (K2) =================================================
//
// The v0.10 corkboard surface. The SPA calls these "anchors"; internally
// the per-kb sqlite table is `corkboard` to avoid collision with the
// stale-anchor sidecar above.
//
// Routes:
//   GET    /api/anchors                            → cross-kb list
//   POST   /api/kb/{kb}/anchors/{artifact_id}      → pin
//   DELETE /api/kb/{kb}/anchors/{artifact_id}      → unpin
//
// SSE: `anchor.added` / `anchor.removed` carry `{kb, artifact_id}`.

#[cfg_attr(feature = "ts-export", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Serialize)]
pub struct CorkboardResponse {
    pub anchors: Vec<kb_core::corkboard::Entry>,
}

#[cfg_attr(
    feature = "ts-export",
    derive(ts_rs::TS),
    ts(export, rename = "AnchorPinResponse")
)]
#[derive(Debug, Serialize)]
pub struct PinResponse {
    /// `true` if a new row was inserted, `false` if the artifact was
    /// already on the corkboard. Idempotent — both 200; the SPA's
    /// optimistic state should ignore the boolean except for stats.
    pub added: bool,
}

/// `GET /api/anchors` — list every pinned artifact across every kb.
/// Joins each row with its lance projection (title / folder / source-
/// relative path); when an artifact has since left lance, the row is
/// still returned but with the projection fields absent — the SPA
/// renders these as "tombstones" so the user can clean them up.
pub async fn list(State(state): State<Arc<KbHandles>>) -> Response<Body> {
    // FF-D — fan out each kb's corkboard concurrently (bounded), flattening in
    // BTreeMap order. Pure reads; one kb's corruption stays logged + skipped.
    let mut futs: Vec<super::CorpusFut<'_, Vec<kb_core::corkboard::Entry>>> = Vec::new();
    for (kb_name, ctx) in state.kbs.iter() {
        futs.push(Box::pin(async move {
            let rows = match ctx.storage.corkboard_list().await {
                Ok(r) => r,
                Err(e) => {
                    tracing::warn!(kb = %kb_name, error = %e, "corkboard_list failed");
                    return Vec::new();
                }
            };
            let mut entries = Vec::new();
            for row in rows {
                let doc = ctx
                    .storage
                    .get_by_id(row.artifact_id.clone())
                    .await
                    .ok()
                    .flatten();
                let (title, source_relative, folder) = match doc {
                    Some(d) => (
                        Some(d.title.clone()),
                        Some(kb_core::paths::doc_rel_path(&d.path, &ctx.source_path)),
                        Some(kb_core::paths::doc_folder(&d.path, &ctx.source_path)),
                    ),
                    None => (None, None, None),
                };
                entries.push(kb_core::corkboard::Entry {
                    kb: kb_name.as_str().to_string(),
                    artifact_id: row.artifact_id,
                    created_at: row.created_at_unix,
                    title,
                    source_relative,
                    folder,
                });
            }
            entries
        }));
    }
    // PF-R1 — the operator-configurable `[server] fanout_cap` (default 8,
    // byte-identical to the old hardcoded `super::FANOUT_CAP`).
    let out: Vec<kb_core::corkboard::Entry> = super::buffered_join(futs, state.fanout_cap)
        .await
        .into_iter()
        .flatten()
        .collect();
    Json(CorkboardResponse { anchors: out }).into_response()
}

/// `POST /api/kb/{kb}/anchors/{artifact_id}` — pin. 200 + `{added}`.
/// 404 if the artifact id is unknown in the kb (we refuse to pin a
/// non-existent artifact — silently accepting it would leave dangling
/// rows the GET endpoint can't project).
pub async fn pin(
    State(state): State<Arc<KbHandles>>,
    Path((kb, artifact_id)): Path<(String, String)>,
) -> Response<Body> {
    let (kb_name, ctx) = match crate::routes::resolve_kb(&state, &kb) {
        Ok(v) => v,
        Err(resp) => return resp,
    };
    // Refuse to pin an artifact that doesn't exist in this kb. A 404
    // here is preferable to a silent insert that the cross-kb GET
    // would later project as a tombstone — the user clearly meant a
    // real artifact, so the typo is their bug to fix, not ours to
    // paper over.
    match ctx.storage.get_by_id(artifact_id.clone()).await {
        Ok(Some(_)) => {}
        Ok(None) => {
            return error_to_problem_json(&kb_core::Error::NotFound(format!(
                "artifact {artifact_id} in kb {kb_name}"
            )))
        }
        Err(e) => return error_to_problem_json(&e),
    }
    let now_unix = chrono::Utc::now().timestamp();
    let added = match ctx
        .storage
        .corkboard_add(artifact_id.clone(), now_unix)
        .await
    {
        Ok(v) => v,
        Err(e) => return error_to_problem_json(&e),
    };
    if added {
        ctx.bus.emit(
            "anchor.added",
            serde_json::json!({
                "kb": kb_name.as_str(),
                "artifact_id": artifact_id,
            }),
        );
    }
    Json(PinResponse { added }).into_response()
}

/// `DELETE /api/kb/{kb}/anchors/{artifact_id}` — unpin. 204 always;
/// idempotent. Emits `anchor.removed` only on an actual delete so
/// SPA subscribers don't refetch on double-clicks.
pub async fn unpin(
    State(state): State<Arc<KbHandles>>,
    Path((kb, artifact_id)): Path<(String, String)>,
) -> Response<Body> {
    let (kb_name, ctx) = match crate::routes::resolve_kb(&state, &kb) {
        Ok(v) => v,
        Err(resp) => return resp,
    };
    let removed = match ctx.storage.corkboard_remove(artifact_id.clone()).await {
        Ok(v) => v,
        Err(e) => return error_to_problem_json(&e),
    };
    if removed {
        ctx.bus.emit(
            "anchor.removed",
            serde_json::json!({
                "kb": kb_name.as_str(),
                "artifact_id": artifact_id,
            }),
        );
    }
    StatusCode::NO_CONTENT.into_response()
}

// === Existing stale-anchor cold-load (Q1) =================================

pub async fn list_stale(State(state): State<Arc<KbHandles>>) -> Response<Body> {
    // Walk every kb in deterministic order (BTreeMap iteration).
    // Each kb's sidecar is independent — a corrupt or missing file in
    // one kb must not poison the others (the loader already returns an
    // empty map on parse failure + logs a warn).
    // FF-D — fan out each kb's sidecar cold-load concurrently (bounded). The
    // sidecar parse is sync; the per-row get_by_id resolution is the await.
    // Each kb's entries are sorted within the future; the fold concatenates in
    // BTreeMap order (cross-kb order unchanged).
    let paths = &state.paths;
    let mut futs: Vec<super::CorpusFut<'_, Vec<StaleAnchorOut>>> = Vec::new();
    for (kb_name, ctx) in state.kbs.iter() {
        futs.push(Box::pin(async move {
            let review_dir = paths.kb_review_dir(kb_name);
            let sidecar = kb_core::anchors::sidecar_path(&review_dir);
            let stale = kb_core::anchors::load(&sidecar);
            // Sort within each kb so the response is stable across calls for
            // tests + diff-friendly caching.
            let mut entries: Vec<((String, String), kb_core::anchors::StaleAnchorEntry)> =
                stale.into_iter().collect();
            entries.sort_by(|a, b| a.0.cmp(&b.0));
            let mut out = Vec::new();
            for ((artifact_id, comment_id), meta) in entries {
                // Resolve the artifact's relative path for the SPA permalink.
                // None when the sidecar references a file that's since left
                // lance (removed/renamed) — the dashboard still shows the row.
                let source_relative = match ctx.storage.get_by_id(artifact_id.clone()).await {
                    Ok(Some(doc)) => {
                        Some(kb_core::paths::doc_rel_path(&doc.path, &ctx.source_path))
                    }
                    _ => None,
                };
                out.push(StaleAnchorOut {
                    kb: kb_name.as_str().to_string(),
                    artifact_id,
                    comment_id,
                    anchor_kind: meta.anchor_kind,
                    fuzzy_score: meta.fuzzy_score,
                    source_relative,
                });
            }
            out
        }));
    }
    // PF-R1 — the operator-configurable `[server] fanout_cap` (default 8,
    // byte-identical to the old hardcoded `super::FANOUT_CAP`).
    let out: Vec<StaleAnchorOut> = super::buffered_join(futs, state.fanout_cap)
        .await
        .into_iter()
        .flatten()
        .collect();
    Json(StaleAnchorsResponse { anchors: out }).into_response()
}
