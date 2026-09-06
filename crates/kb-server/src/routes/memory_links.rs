//! L6 — memory ↔ kb link mutation routes.
//!
//! * `PUT    /api/kb/{kb}/memories/{id}/links`              — atomic replace
//! * `POST   /api/kb/{kb}/memories/{id}/links/{target_kb}`  — add (idempotent)
//! * `DELETE /api/kb/{kb}/memories/{id}/links/{target_kb}`  — remove (404-safe)
//!
//! The home kb must be memory-scoped (the memory file physically lives
//! there). Each named `target_kb` must exist in `state.kbs` — we won't
//! quietly write a link that the recall self-heal will then ignore.
//! Every mutation emits a `memory.linked` / `memory.unlinked` SSE
//! envelope so the SPA's popover stays in sync without manual refresh.

use crate::middleware::error_to_problem_json;
use crate::state::KbHandles;
use axum::{
    body::Body,
    extract::{Path, State},
    http::{Response, StatusCode},
    response::IntoResponse,
    Json,
};
use kb_core::types::KbName;
use serde::{Deserialize, Serialize};
use std::sync::Arc;

/// Sentinel value the storage layer uses for "global" (visible
/// everywhere). Mirrors the V0010 SQL header.
const GLOBAL_SENTINEL: &str = "*";

#[derive(Debug, Deserialize)]
pub struct ReplaceBody {
    /// Explicit kb name list. Empty + `global = false` means the memory
    /// is unlinked (invisible to recall). Each name must correspond to
    /// a kb registered in this daemon.
    #[serde(default)]
    pub linked_kbs: Vec<String>,
    /// When true, also writes the `*` sentinel row so the memory is
    /// recallable from every kb regardless of `linked_kbs`.
    #[serde(default)]
    pub global: bool,
}

#[cfg_attr(
    feature = "ts-export",
    derive(ts_rs::TS),
    ts(export, rename = "MemoryLinks")
)]
#[derive(Debug, Serialize)]
pub struct LinksResponse {
    /// True when the `*` sentinel row is present.
    pub global: bool,
    /// All non-sentinel kb names, sorted asc. Empty when the memory has
    /// no explicit links.
    pub linked_kbs: Vec<String>,
}

/// PUT /api/kb/{kb}/memories/{id}/links — atomic replace of the entire
/// link set for one memory. Returns the persisted state.
pub async fn replace(
    State(state): State<Arc<KbHandles>>,
    Path((kb, artifact_id)): Path<(String, String)>,
    Json(body): Json<ReplaceBody>,
) -> Response<Body> {
    let (kb_name, ctx) = match crate::routes::resolve_kb(&state, &kb) {
        Ok(v) => v,
        Err(resp) => return resp,
    };
    if ctx.memory_scope.is_none() {
        return error_to_problem_json(&kb_core::Error::BadRequest(format!(
            "kb {kb_name} is not a memory corpus; link mutations only apply to memory-scoped kbs"
        )));
    }

    // Validate target kb names — known + non-sentinel. We surface
    // unknown names as 400 rather than silently dropping them at
    // recall time (orphan rows ARE tolerated for kbs the operator
    // removed mid-flight, but a brand-new link to a typoed name is a
    // user error worth catching).
    let mut clean: Vec<String> = Vec::with_capacity(body.linked_kbs.len());
    for k in &body.linked_kbs {
        if k == GLOBAL_SENTINEL {
            return error_to_problem_json(&kb_core::Error::BadRequest(format!(
                "linked_kbs: '{GLOBAL_SENTINEL}' is reserved; use the `global` flag instead"
            )));
        }
        let target = match KbName::new(k) {
            Ok(k) => k,
            Err(e) => return error_to_problem_json(&e),
        };
        if !state.kbs.contains_key(&target) {
            return error_to_problem_json(&kb_core::Error::BadRequest(format!(
                "linked_kbs: unknown kb '{k}'"
            )));
        }
        clean.push(target.as_str().to_string());
    }

    // Existence check on the home memory artifact — a 404 here is
    // friendlier than a successful PUT against a dangling id.
    match ctx.storage.get_by_id(artifact_id.clone()).await {
        Ok(Some(_)) => {}
        Ok(None) => {
            return error_to_problem_json(&kb_core::Error::NotFound(format!(
                "artifact {artifact_id} in kb {kb_name}"
            )))
        }
        Err(e) => return error_to_problem_json(&e),
    }

    let now = chrono::Utc::now().timestamp();
    if let Err(e) = ctx
        .storage
        .memory_links_replace(artifact_id.clone(), clean.clone(), body.global, now)
        .await
    {
        return error_to_problem_json(&e);
    }

    // SSE — one event per route (not per row). Carries the full new
    // set so a subscriber doesn't need to re-fetch.
    ctx.bus.emit(
        "memory.linked",
        serde_json::json!({
            "kb": kb_name.as_str(),
            "id": artifact_id,
            "global": body.global,
            "linked_kbs": clean,
        }),
    );

    let mut linked_kbs = clean;
    linked_kbs.sort();
    Json(LinksResponse {
        global: body.global,
        linked_kbs,
    })
    .into_response()
}

/// POST /api/kb/{kb}/memories/{id}/links/{target_kb} — add one link.
/// Idempotent. `target_kb = '*'` is rejected; use PUT with `global: true`.
pub async fn add(
    State(state): State<Arc<KbHandles>>,
    Path((kb, artifact_id, target_kb)): Path<(String, String, String)>,
) -> Response<Body> {
    let (kb_name, ctx) = match crate::routes::resolve_kb(&state, &kb) {
        Ok(v) => v,
        Err(resp) => return resp,
    };
    if ctx.memory_scope.is_none() {
        return error_to_problem_json(&kb_core::Error::BadRequest(format!(
            "kb {kb_name} is not a memory corpus"
        )));
    }
    if target_kb == GLOBAL_SENTINEL {
        return error_to_problem_json(&kb_core::Error::BadRequest(
            "use PUT /links with `global: true` to mark a memory global".into(),
        ));
    }
    let target = match KbName::new(&target_kb) {
        Ok(k) => k,
        Err(e) => return error_to_problem_json(&e),
    };
    if !state.kbs.contains_key(&target) {
        return error_to_problem_json(&kb_core::Error::BadRequest(format!(
            "unknown kb '{target_kb}'"
        )));
    }

    let now = chrono::Utc::now().timestamp();
    match ctx
        .storage
        .memory_link_add(artifact_id.clone(), target.as_str().to_string(), now)
        .await
    {
        Ok(_) => {
            ctx.bus.emit(
                "memory.linked",
                serde_json::json!({
                    "kb": kb_name.as_str(),
                    "id": artifact_id,
                    "added": target.as_str(),
                }),
            );
            StatusCode::NO_CONTENT.into_response()
        }
        Err(e) => error_to_problem_json(&e),
    }
}

/// DELETE /api/kb/{kb}/memories/{id}/links/{target_kb} — remove one
/// link. 204; idempotent (also 204 when the link wasn't present).
pub async fn remove(
    State(state): State<Arc<KbHandles>>,
    Path((kb, artifact_id, target_kb)): Path<(String, String, String)>,
) -> Response<Body> {
    let (kb_name, ctx) = match crate::routes::resolve_kb(&state, &kb) {
        Ok(v) => v,
        Err(resp) => return resp,
    };
    if ctx.memory_scope.is_none() {
        return error_to_problem_json(&kb_core::Error::BadRequest(format!(
            "kb {kb_name} is not a memory corpus"
        )));
    }
    // Allow the `*` sentinel here — operators may want to clear the
    // "global" flag via DELETE rather than PUT.
    if target_kb != GLOBAL_SENTINEL {
        let target = match KbName::new(&target_kb) {
            Ok(k) => k,
            Err(e) => return error_to_problem_json(&e),
        };
        // Unknown kb names DON'T 400 on delete — let operators clean
        // up orphan rows after removing a kb from kb.toml.
        let _ = target;
    }

    match ctx
        .storage
        .memory_link_remove(artifact_id.clone(), target_kb.clone())
        .await
    {
        Ok(_) => {
            ctx.bus.emit(
                "memory.unlinked",
                serde_json::json!({
                    "kb": kb_name.as_str(),
                    "id": artifact_id,
                    "removed": target_kb,
                }),
            );
            StatusCode::NO_CONTENT.into_response()
        }
        Err(e) => error_to_problem_json(&e),
    }
}

/// GET /api/kb/{kb}/memories/{id}/links — read the current link set.
/// Convenience for the SPA popover when it doesn't already have the
/// info from the recall response. Returns `LinksResponse`.
pub async fn get(
    State(state): State<Arc<KbHandles>>,
    Path((kb, artifact_id)): Path<(String, String)>,
) -> Response<Body> {
    let (kb_name, ctx) = match crate::routes::resolve_kb(&state, &kb) {
        Ok(v) => v,
        Err(resp) => return resp,
    };
    if ctx.memory_scope.is_none() {
        return error_to_problem_json(&kb_core::Error::BadRequest(format!(
            "kb {kb_name} is not a memory corpus"
        )));
    }
    match ctx.storage.memory_links_for(artifact_id).await {
        Ok(rows) => {
            let mut global = false;
            let mut linked_kbs: Vec<String> = Vec::with_capacity(rows.len());
            for r in rows {
                if r == GLOBAL_SENTINEL {
                    global = true;
                } else {
                    linked_kbs.push(r);
                }
            }
            linked_kbs.sort();
            Json(LinksResponse { global, linked_kbs }).into_response()
        }
        Err(e) => error_to_problem_json(&e),
    }
}
