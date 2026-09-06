//! `POST /api/kb/{kb}/sources/{src}/reindex` — async reindex command
//! (topic 11 §B.3). Returns 202 + `{run, events}` per the async pattern.
//!
//! v0.0.1 implementation: walks the source folder via the EventBus by
//! emitting synthetic `watch.create` envelopes (the indexer treats them
//! the same as live FS events and re-indexes idempotently).

use crate::middleware::error_to_problem_json;
use crate::state::KbHandles;
use axum::{
    body::Body,
    extract::{Path, State},
    http::{Response, StatusCode},
    response::IntoResponse,
    Json,
};
use kb_core::ids::RunId;
use serde::Serialize;
use std::sync::Arc;

#[derive(Debug, Serialize)]
pub struct ReindexResponse {
    pub run: String,
    pub events: String,
}

pub async fn post(
    State(state): State<Arc<KbHandles>>,
    Path((kb, src)): Path<(String, String)>,
) -> Response<Body> {
    let (_kb_name, ctx) = match crate::routes::resolve_kb(&state, &kb) {
        Ok(v) => v,
        Err(resp) => return resp,
    };

    if src != ctx.source_slug.as_str() {
        let err = kb_core::Error::NotFound(format!("source {src}"));
        return error_to_problem_json(&err);
    }

    spawn_walk(ctx).await
}

/// v0.6 — kb-level reindex shortcut. Each kb currently has a single
/// source so this is just the per-source endpoint without requiring
/// the caller to know the slug. Used by the SPA's "populate v0.6
/// fields" affordance + by anyone scripting against the API.
///
/// LOW (deep-review): SINGLE-SOURCE ASSUMPTION marker. When kb gains
/// multi-source-per-kb (no current milestone owns this), this route
/// either needs a `src` parameter or has to walk every configured
/// source. Today `spawn_walk` walks `ctx.source_path` (the kb's lone
/// source). If a second source were added without revisiting this
/// route, only the FIRST would reindex on `POST /api/kb/{kb}/reindex`.
pub async fn kb_post(
    State(state): State<Arc<KbHandles>>,
    Path(kb): Path<String>,
) -> Response<Body> {
    let (_kb_name, ctx) = match crate::routes::resolve_kb(&state, &kb) {
        Ok(v) => v,
        Err(resp) => return resp,
    };
    spawn_walk(ctx).await
}

async fn spawn_walk(ctx: &crate::state::KbContext) -> Response<Body> {
    let run = RunId::new();

    // Spawn a background walk that pushes a force `WatchWork` for each
    // existing artifact through the ingest sink. The indexer drains the
    // sink's channel and re-indexes. walkdir is sync — `spawn_blocking`
    // keeps the tokio executor responsive, and the sink's `blocking_send`
    // back-pressures the walk so a corpus larger than the channel can't
    // drop work (G7: the force path no longer overflows the broadcast).
    //
    // `skip_patterns` are threaded from the kb's config so the operator's
    // "don't index this" rule is honoured on explicit reindex too — not
    // just on live-watcher events.
    let ingest = ctx.ingest.clone();
    let path = ctx.source_path.clone();
    let skips = ctx.skip_patterns.clone();
    tokio::task::spawn_blocking(move || {
        // `force = true`: explicit operator reindex bypasses the indexer's
        // byte-identical dedup gate. Without this, files whose content is
        // unchanged since the last index never re-run the parse/embed/
        // edge-resolve pipeline — and links that originally didn't resolve
        // (because the target wasn't indexed yet) stay un-recorded forever.
        // No mtime map: the operator wants the full pipeline for every file.
        kb_core::indexer::walk_send_work(&ingest, &path, &skips, true, None);
    });

    let mut resp = (
        StatusCode::ACCEPTED,
        Json(ReindexResponse {
            run: run.to_string(),
            events: format!("/api/events?filter=run:{run}"),
        }),
    )
        .into_response();
    resp.headers_mut().insert(
        axum::http::header::CONTENT_TYPE,
        axum::http::HeaderValue::from_static("application/json"),
    );
    resp
}
