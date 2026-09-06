//! `POST /api/kb/{kb}/compact` — operator-triggered lance maintenance.
//! Runs `Table::optimize(OptimizeAction::All)` through the storage
//! actor: merges small data fragments, rebuilds indices over rows
//! added since the last optimize, and prunes manifest versions
//! eligible for cleanup. Returns 200 + `CompactStats` JSON when the
//! actor completes the pass.
//!
//! Why an explicit endpoint instead of folding this into reindex: the
//! indexer pipeline upserts one document per file, so each reindex
//! commits N fragments + N manifest versions. Bundling optimize into
//! reindex would double the wall-time of every reindex even when the
//! dataset is already healthy. The startup auto-compact heuristic
//! (`crate::lib::spawn_startup_compact`) covers the common case; this
//! route is the manual repair lever the CLI verb `kb compact` drives.
//!
//! Single-writer-safety: kb-core::storage::actor serialises every
//! mutation behind one `mpsc::channel`, so the compact request waits
//! its turn behind any in-flight upsert and no concurrent write can
//! race the lance optimize pass.

use crate::middleware::error_to_problem_json;
use crate::state::KbHandles;
use axum::{
    body::Body,
    extract::{Path, State},
    http::Response,
    response::IntoResponse,
    Json,
};
use std::sync::Arc;
use std::time::Instant;

pub async fn post(State(state): State<Arc<KbHandles>>, Path(kb): Path<String>) -> Response<Body> {
    let (kb_name, ctx) = match crate::routes::resolve_kb(&state, &kb) {
        Ok(v) => v,
        Err(resp) => return resp,
    };

    let started = Instant::now();
    let stats = match ctx.storage.compact_all().await {
        Ok(s) => s,
        Err(e) => return error_to_problem_json(&e),
    };
    let ms = started.elapsed().as_millis() as u64;

    // Bus emit so the TUI EVENTS tab + any /api/events subscribers see
    // the maintenance pass land. Mirrors the `query` event shape from
    // `routes::search` (one envelope per completed action).
    ctx.bus.emit(
        "maintenance.compact.done",
        serde_json::json!({
            "kb": kb_name.as_str(),
            "ms": ms,
            "trigger": "route",
            "stats": &stats,
        }),
    );

    Json(serde_json::json!({
        "ms": ms,
        "stats": stats,
    }))
    .into_response()
}
