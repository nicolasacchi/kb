//! `GET /api/kb/{kb}/sources` (list, topic 11 §B.2) +
//! `POST /api/kb/{kb}/sources/{src}/pause` +
//! `POST /api/kb/{kb}/sources/{src}/resume` (v0.1, §B.3).

use crate::middleware::error_to_problem_json;
use crate::state::KbHandles;
use axum::{
    body::Body,
    extract::{Path, Query, State},
    http::Response,
    response::IntoResponse,
    Json,
};
use kb_core::ids::SourceSlug;
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::sync::Arc;

#[cfg_attr(feature = "ts-export", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Serialize)]
pub struct SourceSummary {
    pub slug: String,
    pub path: String,
    pub paused: bool,
    pub doc_count: u64,
}

pub async fn list(State(state): State<Arc<KbHandles>>, Path(kb): Path<String>) -> Response<Body> {
    let (_kb_name, ctx) = match crate::routes::resolve_kb(&state, &kb) {
        Ok(v) => v,
        Err(resp) => return resp,
    };

    let sources = match ctx.storage.list_sources().await {
        Ok(s) => s,
        Err(e) => return error_to_problem_json(&e),
    };

    let doc_count = ctx.storage.count_rows().await.unwrap_or(0);

    let summaries = sources
        .into_iter()
        .map(|s| SourceSummary {
            slug: s.raw_slug,
            path: s.path.to_string_lossy().to_string(),
            paused: s.paused,
            // v0.0.1 has one source per kb; doc_count is per-kb not per-source.
            doc_count,
        })
        .collect::<Vec<_>>();

    Json(summaries).into_response()
}

pub async fn pause(
    State(state): State<Arc<KbHandles>>,
    Path((kb, src)): Path<(String, String)>,
) -> Response<Body> {
    set_paused(&state, &kb, &src, true).await
}

pub async fn resume(
    State(state): State<Arc<KbHandles>>,
    Path((kb, src)): Path<(String, String)>,
) -> Response<Body> {
    set_paused(&state, &kb, &src, false).await
}

/// `GET /api/kb/{kb}/runs?limit=N` — v0.6 R1. Newest-first list of
/// recent indexing runs from the per-kb runs ring. `limit` defaults
/// to 50; clamped to `[1, 256]`.
#[derive(Debug, Deserialize)]
pub struct LimitQuery {
    pub limit: Option<usize>,
}

const RUNS_DEFAULT_LIMIT: usize = 50;
const RUNS_MAX_LIMIT: usize = kb_core::history::DEFAULT_CAPACITY;

pub async fn runs_list(
    State(state): State<Arc<KbHandles>>,
    Path(kb): Path<String>,
    Query(LimitQuery { limit }): Query<LimitQuery>,
) -> Response<Body> {
    let (_kb_name, ctx) = match crate::routes::resolve_kb(&state, &kb) {
        Ok(v) => v,
        Err(resp) => return resp,
    };
    let n = limit.unwrap_or(RUNS_DEFAULT_LIMIT).clamp(1, RUNS_MAX_LIMIT);
    Json(ctx.runs.snapshot(n)).into_response()
}

async fn set_paused(state: &Arc<KbHandles>, kb: &str, src: &str, paused: bool) -> Response<Body> {
    let (kb_name, ctx) = match crate::routes::resolve_kb(state, kb) {
        Ok(v) => v,
        Err(resp) => return resp,
    };
    if src != ctx.source_slug.as_str() {
        let err = kb_core::Error::NotFound(format!("source {src}"));
        return error_to_problem_json(&err);
    }
    let slug: SourceSlug = match serde_json::from_value(serde_json::Value::String(src.to_string()))
    {
        Ok(s) => s,
        Err(_) => {
            let err = kb_core::Error::BadRequest(format!("invalid source slug {src:?}"));
            return error_to_problem_json(&err);
        }
    };
    if let Err(e) = ctx.storage.set_source_paused(slug, paused).await {
        return error_to_problem_json(&e);
    }
    // X2/D6 — flip the LIVE ingest gate too (sqlite is the durable truth the
    // gate reloads from at bring-up; the gate is what the walk/watcher/
    // prepare_doc seams actually enforce). Pre-v0.24 `paused` was stored but
    // never enforced; now pausing genuinely stops ingest until resume.
    ctx.ingest.gate().set_paused(paused);
    let evt = if paused {
        "source.paused"
    } else {
        "source.resumed"
    };
    ctx.bus
        .emit(evt, json!({"kb": kb_name.as_str(), "src": src}));
    Json(json!({
        "src": src,
        "paused": paused,
    }))
    .into_response()
}
