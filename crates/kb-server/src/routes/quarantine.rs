//! Quarantine inspection + un-quarantine workflow.
//!
//! - `GET  /api/kb/{kb}/quarantine`               — list quarantined artifacts
//! - `POST /api/kb/{kb}/quarantine/restore`       — restore one (body: `{path}`)
//! - `POST /api/kb/{kb}/quarantine/restore-all`   — bulk
//!
//! Listing source: open `errors` rows with `retry_count >= QUARANTINE_THRESHOLD - 1`
//! (matches the indexer's `record_failure` trigger). Each entry is enriched
//! with whether a `<state>/quarantine/<kb>/{stem}.html` sidecar exists.
//!
//! Restore: clears all open error rows for the path, deletes the sidecar
//! pair on disk, emits `quarantine.restored` SSE, then re-emits a
//! `watch.modify` envelope with `force=true` so the indexer retries
//! immediately (bypassing the hash-dedup gate).

use crate::middleware::error_to_problem_json;
use crate::state::KbHandles;
use axum::{
    body::Body,
    extract::{Path, State},
    http::Response,
    response::IntoResponse,
    Json,
};
use kb_core::indexer::QUARANTINE_THRESHOLD;
use kb_core::types::KbName;
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::path::PathBuf;
use std::sync::Arc;

#[derive(Debug, Serialize)]
pub struct QuarantineEntry {
    pub error_id: String,
    pub kind: String,
    pub source_slug: String,
    pub path: String,
    pub message: String,
    pub retry_count: u32,
    pub created_at: i64,
    /// True when `<state>/quarantine/<kb>/{stem}.html` exists on disk.
    /// May be false if the sidecar was hand-deleted but the error rows
    /// remained — restoring is still useful (just clears the DB).
    pub sidecar_present: bool,
}

pub async fn list(State(state): State<Arc<KbHandles>>, Path(kb): Path<String>) -> Response<Body> {
    let (kb_name, ctx) = match crate::routes::resolve_kb(&state, &kb) {
        Ok(v) => v,
        Err(resp) => return resp,
    };

    let rows = match ctx.storage.list_open_errors().await {
        Ok(r) => r,
        Err(e) => return error_to_problem_json(&e),
    };

    let qdir = state.paths.quarantine_kb_dir(&kb_name);
    let entries: Vec<QuarantineEntry> = rows
        .into_iter()
        .filter(|r| r.retry_count + 1 >= QUARANTINE_THRESHOLD)
        .map(|r| {
            let sidecar_present = sidecar_path(&qdir, &r.path).is_some_and(|p| p.exists());
            QuarantineEntry {
                error_id: r.id,
                kind: r.kind,
                source_slug: r.source_slug,
                path: r.path.to_string_lossy().to_string(),
                message: r.message,
                retry_count: r.retry_count,
                created_at: r.created_at_unix,
                sidecar_present,
            }
        })
        .collect();
    Json(entries).into_response()
}

#[derive(Debug, Deserialize)]
pub struct RestoreBody {
    pub path: String,
}

#[derive(Debug, Serialize)]
pub struct RestoreResult {
    pub path: String,
    pub errors_cleared: usize,
    pub sidecar_removed: bool,
}

pub async fn restore(
    State(state): State<Arc<KbHandles>>,
    Path(kb): Path<String>,
    Json(body): Json<RestoreBody>,
) -> Response<Body> {
    let (kb_name, ctx) = match crate::routes::resolve_kb(&state, &kb) {
        Ok(v) => v,
        Err(resp) => return resp,
    };
    let path = PathBuf::from(&body.path);
    let result = match do_restore(&state, &kb_name, &path).await {
        Ok(r) => r,
        Err(e) => return error_to_problem_json(&e),
    };

    ctx.bus.emit(
        "quarantine.restored",
        json!({"kb": kb_name.as_str(), "path": result.path}),
    );
    nudge_indexer(&ctx.ingest, &path).await;

    Json(result).into_response()
}

#[derive(Debug, Serialize)]
pub struct RestoreAllResult {
    pub kb: String,
    pub restored: Vec<RestoreResult>,
}

pub async fn restore_all(
    State(state): State<Arc<KbHandles>>,
    Path(kb): Path<String>,
) -> Response<Body> {
    let (kb_name, ctx) = match crate::routes::resolve_kb(&state, &kb) {
        Ok(v) => v,
        Err(resp) => return resp,
    };
    let rows = match ctx.storage.list_open_errors().await {
        Ok(r) => r,
        Err(e) => return error_to_problem_json(&e),
    };
    let paths: Vec<PathBuf> = rows
        .into_iter()
        .filter(|r| r.retry_count + 1 >= QUARANTINE_THRESHOLD)
        .map(|r| r.path)
        .collect();
    let mut out = Vec::with_capacity(paths.len());
    for path in paths {
        match do_restore(&state, &kb_name, &path).await {
            Ok(r) => {
                ctx.bus.emit(
                    "quarantine.restored",
                    json!({"kb": kb_name.as_str(), "path": r.path}),
                );
                nudge_indexer(&ctx.ingest, &path).await;
                out.push(r);
            }
            Err(e) => {
                tracing::warn!(
                    kb = %kb_name,
                    path = %path.display(),
                    error = %e,
                    "quarantine restore-all: per-path failure (skipping)",
                );
            }
        }
    }
    Json(RestoreAllResult {
        kb: kb_name.as_str().to_string(),
        restored: out,
    })
    .into_response()
}

async fn do_restore(
    state: &Arc<KbHandles>,
    kb_name: &KbName,
    path: &std::path::Path,
) -> kb_core::Result<RestoreResult> {
    let ctx = state
        .kbs
        .get(kb_name)
        .ok_or_else(|| kb_core::Error::NotFound(format!("kb {kb_name}")))?;

    let errors_cleared = ctx
        .storage
        .clear_errors_for_path(path.to_path_buf())
        .await?;

    let qdir = state.paths.quarantine_kb_dir(kb_name);
    let mut sidecar_removed = false;
    if let Some(html) = sidecar_path(&qdir, path) {
        let txt = html.with_extension("error.txt");
        let removed_html = std::fs::remove_file(&html).is_ok();
        let removed_txt = std::fs::remove_file(&txt).is_ok();
        sidecar_removed = removed_html || removed_txt;
    }

    Ok(RestoreResult {
        path: path.to_string_lossy().to_string(),
        errors_cleared,
        sidecar_removed,
    })
}

/// Push a `force` re-index for the file through the ingest sink so the indexer
/// retries it immediately, bypassing the content-hash dedup gate (the bytes
/// haven't changed since the last failure). G7: routed through the sink (not a
/// bare `watch.modify` bus emit) so it reaches the indexer's channel — the
/// indexer no longer reads `watch.*` off the bus. The sink mirrors to the bus
/// too, so observers still see the `watch.modify`.
async fn nudge_indexer(ingest: &kb_core::indexer::IngestSink, path: &std::path::Path) {
    ingest
        .send(
            kb_core::indexer::WatchKind::Modified,
            path.to_path_buf(),
            true,
        )
        .await;
}

/// Resolve `<quarantine-dir>/<stem>.html` from the original source path's
/// file stem — the same path `kb_core::indexer::quarantine()` writes to.
fn sidecar_path(qdir: &std::path::Path, original: &std::path::Path) -> Option<PathBuf> {
    let stem = original.file_stem()?.to_str()?;
    Some(qdir.join(format!("{stem}.html")))
}
