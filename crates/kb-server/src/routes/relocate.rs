//! F3b — HTTP surface for the relocate engine (`kb_core::relocate`).
//!
//! - `POST /api/kb/{kb}/docs/{id}/move` — single-artifact rename
//! - `POST /api/kb/{kb}/folders/rename` — batch folder prefix rewrite
//!
//! Auth posture matches other mutating routes (bearer + loopback bypass via
//! the shared `/api` layers). After a successful move the handler emits the
//! existing `artifact.removed` + `artifact.indexed` pair so the SPA SSE
//! bridge (docsGate) invalidates gallery/folders/docs queries without a new
//! event type.

use crate::middleware::error_to_problem_json;
use crate::state::{KbContext, KbHandles};
use axum::{
    body::Body,
    extract::{Path, State},
    http::Response,
    response::IntoResponse,
    Json,
};
use kb_core::paths::doc_rel_path;
use kb_core::relocate::{self, RelocateCtx, RelocateOutcome};
use kb_core::types::KbName;
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::sync::Arc;

#[derive(Debug, Deserialize)]
pub struct MoveBody {
    /// New source-root-relative path (forward slashes).
    pub to: String,
}

#[derive(Debug, Deserialize)]
pub struct FolderRenameBody {
    pub from: String,
    pub to: String,
}

#[derive(Debug, Serialize)]
pub struct MoveResponse {
    pub old_id: String,
    pub new_id: String,
    pub old_source_rel: String,
    pub new_source_rel: String,
}

#[derive(Debug, Serialize)]
pub struct FolderRenameResponse {
    pub moved: Vec<MoveResponse>,
}

impl From<RelocateOutcome> for MoveResponse {
    fn from(o: RelocateOutcome) -> Self {
        Self {
            old_id: o.old_id,
            new_id: o.new_id,
            old_source_rel: o.old_rel,
            new_source_rel: o.new_rel,
        }
    }
}

/// Build a [`RelocateCtx`] for one kb. Passes the shared indexer
/// [`kb_core::indexer::DedupCache`] so relocate rekeys `old_id → new_id`
/// and the post-rename `Created` event hits the content-hash pre-gate
/// (no redundant re-embed; invariant #27 / FU1).
fn build_ctx(state: &KbHandles, kb_name: &KbName, ctx: &KbContext) -> RelocateCtx {
    RelocateCtx {
        storage: ctx.storage.clone(),
        source_root: ctx.source_path.clone(),
        review_dir: state.paths.kb_review_dir(kb_name),
        // Same per-kb lock comment routes use (invariant #6).
        review_lock: Some(state.review_lock_for(kb_name)),
        pending: relocate::shared_pending(),
        dedup: Some(ctx.dedup.clone()),
        kb: kb_name.clone(),
    }
}

/// Emit the bridge-compatible removed+indexed pair for one successful move,
/// plus one `list.updated` per reading list whose entries were rekeyed
/// (SPA lists cache keys on `list.updated {kb,id}`).
async fn emit_move_events(ctx: &KbContext, kb_name: &KbName, out: &RelocateOutcome) {
    let old_abs = ctx.source_path.join(&out.old_rel);
    let new_abs = ctx.source_path.join(&out.new_rel);
    ctx.bus.emit(
        "artifact.removed",
        json!({
            "artifact_id": out.old_id,
            "kb": kb_name.as_str(),
            "path": old_abs.to_string_lossy(),
        }),
    );
    ctx.bus.emit(
        "artifact.indexed",
        json!({
            "artifact_id": out.new_id,
            "kb": kb_name.as_str(),
            "path": new_abs.to_string_lossy(),
            "change_kind": "moved",
        }),
    );
    // Match lists.rs payload shape: {kb, id, title}.
    for list_id in &out.affected_list_ids {
        let title = match ctx.storage.list_get(list_id.clone()).await {
            Ok(Some(row)) => row.title,
            _ => String::new(),
        };
        ctx.bus.emit(
            "list.updated",
            json!({
                "kb": kb_name.as_str(),
                "id": list_id,
                "title": title,
            }),
        );
    }
}

/// `POST /api/kb/{kb}/docs/{id}/move` body `{"to":"<new source-rel>"}`.
pub async fn move_doc(
    State(state): State<Arc<KbHandles>>,
    Path((kb, id)): Path<(String, String)>,
    Json(body): Json<MoveBody>,
) -> Response<Body> {
    let (kb_name, ctx) = match crate::routes::resolve_kb(&state, &kb) {
        Ok(v) => v,
        Err(resp) => return resp,
    };
    if !crate::routes::is_safe_id(&id) {
        return error_to_problem_json(&kb_core::Error::BadRequest(format!(
            "invalid artifact id {id:?}"
        )));
    }

    let row = match ctx.storage.get_by_id(id.clone()).await {
        Ok(Some(r)) => r,
        Ok(None) => {
            return error_to_problem_json(&kb_core::Error::NotFound(format!("doc {id}")));
        }
        Err(e) => return error_to_problem_json(&e),
    };
    let old_rel = doc_rel_path(&row.path, &ctx.source_path);
    if old_rel.is_empty() {
        return error_to_problem_json(&kb_core::Error::BadRequest(format!(
            "could not derive source-relative path for {id}"
        )));
    }

    let rctx = build_ctx(&state, &kb_name, ctx);
    match relocate::relocate_doc(&rctx, &old_rel, &body.to).await {
        Ok(out) => {
            emit_move_events(ctx, &kb_name, &out).await;
            Json(MoveResponse::from(out)).into_response()
        }
        Err(e) => error_to_problem_json(&e),
    }
}

/// `POST /api/kb/{kb}/folders/rename` body `{"from":"…","to":"…"}`.
///
/// Stops on first item failure (engine contract). Completed items are fully
/// consistent per-item; partial batches leave those renames in place.
pub async fn rename_folder(
    State(state): State<Arc<KbHandles>>,
    Path(kb): Path<String>,
    Json(body): Json<FolderRenameBody>,
) -> Response<Body> {
    let (kb_name, ctx) = match crate::routes::resolve_kb(&state, &kb) {
        Ok(v) => v,
        Err(resp) => return resp,
    };

    let from = body.from.trim().trim_end_matches('/');
    let to = body.to.trim().trim_end_matches('/');
    if from.is_empty() || to.is_empty() {
        return error_to_problem_json(&kb_core::Error::BadRequest(
            "folder rename requires non-empty from and to".into(),
        ));
    }

    let rctx = build_ctx(&state, &kb_name, ctx);
    match relocate::relocate_folder(&rctx, from, to).await {
        Ok(items) => {
            if items.is_empty() {
                return error_to_problem_json(&kb_core::Error::NotFound(format!(
                    "no indexed docs under folder {from:?}"
                )));
            }
            let mut moved = Vec::with_capacity(items.len());
            for item in items {
                emit_move_events(ctx, &kb_name, &item.outcome).await;
                moved.push(MoveResponse::from(item.outcome));
            }
            Json(FolderRenameResponse { moved }).into_response()
        }
        Err(e) => {
            // Mid-batch failures: earlier items stay fully consistent
            // (engine is per-item atomic). Annotate validation-class errors
            // so the problem+json detail says so.
            match e {
                kb_core::Error::NotFound(msg) => {
                    error_to_problem_json(&kb_core::Error::NotFound(format!(
                        "{msg} (folder rename stops on first failure; \
                         already-moved items remain fully consistent)"
                    )))
                }
                kb_core::Error::BadRequest(msg) => {
                    error_to_problem_json(&kb_core::Error::BadRequest(format!(
                        "{msg} (folder rename stops on first failure; \
                         already-moved items remain fully consistent)"
                    )))
                }
                kb_core::Error::Conflict(msg) => {
                    error_to_problem_json(&kb_core::Error::Conflict(format!(
                        "{msg} (folder rename stops on first failure; \
                         already-moved items remain fully consistent)"
                    )))
                }
                other => error_to_problem_json(&other),
            }
        }
    }
}
