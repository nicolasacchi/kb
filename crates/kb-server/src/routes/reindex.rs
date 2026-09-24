//! `POST /api/kb/{kb}/sources/{src}/reindex` — async reindex command
//! (topic 11 §B.3). Returns 202 + `{run, events}` per the async pattern.
//!
//! v0.0.1 implementation: walks the source folder via the EventBus by
//! emitting synthetic `watch.create` envelopes (the indexer treats them
//! the same as live FS events and re-indexes idempotently).

use crate::middleware::error_to_problem_json;
use crate::state::KbHandles;
use axum::{
    body::{Body, Bytes},
    extract::{Path, Query, State},
    http::{Response, StatusCode},
    response::IntoResponse,
    Json,
};
use kb_core::ids::RunId;
use serde::{Deserialize, Serialize};
use std::sync::Arc;

#[derive(Debug, Serialize)]
pub struct ReindexResponse {
    pub run: String,
    pub events: String,
}

pub async fn post(
    State(state): State<Arc<KbHandles>>,
    Path((kb, src)): Path<(String, String)>,
    Query(params): Query<ReindexParams>,
    body: Bytes,
) -> Response<Body> {
    let (_kb_name, ctx) = match crate::routes::resolve_kb(&state, &kb) {
        Ok(v) => v,
        Err(resp) => return resp,
    };

    if src != ctx.source_slug.as_str() {
        let err = kb_core::Error::NotFound(format!("source {src}"));
        return error_to_problem_json(&err);
    }

    let re_embed = match re_embed_requested(&params, &body) {
        Ok(v) => v,
        Err(resp) => return *resp,
    };
    spawn_walk(ctx, re_embed).await
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
    Query(params): Query<ReindexParams>,
    body: Bytes,
) -> Response<Body> {
    let (_kb_name, ctx) = match crate::routes::resolve_kb(&state, &kb) {
        Ok(v) => v,
        Err(resp) => return resp,
    };
    let re_embed = match re_embed_requested(&params, &body) {
        Ok(v) => v,
        Err(resp) => return *resp,
    };
    spawn_walk(ctx, re_embed).await
}

/// `re_embed` query param or JSON field. Either `true` forces a real embed.
/// Absent body and absent query keep the default: reuse stored vectors.
///
/// The CLI (`crates/kb-cli/src/commands/reindex.rs`, wired from
/// `crates/kb-cli/src/main.rs`) POSTs with no query and no body, so it gets
/// reuse. A `--re-embed` flag would require those files; this route param
/// is the hatch.
#[derive(Debug, Default, Deserialize)]
pub struct ReindexParams {
    #[serde(default)]
    re_embed: bool,
}

fn re_embed_requested(query: &ReindexParams, body: &[u8]) -> Result<bool, Box<Response<Body>>> {
    if body.iter().all(|b| b.is_ascii_whitespace()) {
        return Ok(query.re_embed);
    }
    let parsed: ReindexParams = serde_json::from_slice(body).map_err(|e| {
        Box::new(error_to_problem_json(&kb_core::Error::BadRequest(format!(
            "reindex body: {e}"
        ))))
    })?;
    Ok(query.re_embed || parsed.re_embed)
}

async fn spawn_walk(ctx: &crate::state::KbContext, re_embed: bool) -> Response<Body> {
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
        // byte-identical early return so parse, edges, anchors, and
        // coderefs re-run. It does not re-embed when the content hash,
        // model, and dim match — `re_embed` is that hatch (perf-04).
        // No mtime map: the operator wants the parse pipeline for every file.
        kb_core::indexer::walk_send_reindex(&ingest, &path, &skips, re_embed);
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
