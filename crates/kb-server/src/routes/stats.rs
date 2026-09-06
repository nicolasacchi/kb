//! `GET /api/stats` (cross-kb) and `GET /api/kb/{kb}/stats` (per-kb).
//! Aggregates from sqlite + lance row counts.

use crate::state::KbHandles;
use axum::{
    body::Body,
    extract::{Path, State},
    http::Response,
    response::IntoResponse,
    Json,
};
use serde::Serialize;
use std::sync::Arc;

#[cfg_attr(feature = "ts-export", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Serialize)]
pub struct CrossStats {
    pub daemon: DaemonStats,
    pub kbs: Vec<KbStats>,
    pub total_docs: u64,
    pub total_open_errors: u64,
}

#[cfg_attr(feature = "ts-export", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Serialize)]
pub struct DaemonStats {
    pub name: String,
    pub started_at: String,
}

#[cfg_attr(feature = "ts-export", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Serialize)]
pub struct KbStats {
    pub name: String,
    pub doc_count: u64,
    pub open_errors: u64,
    pub last_index_at: Option<i64>,
    /// R1 — wall-clock unix seconds of the most recent reconciler
    /// pass. `None` until the first pass completes; stays `None`
    /// forever when `reconcile_secs = 0`.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts-export", ts(optional))]
    pub last_reconcile_at: Option<i64>,
    /// R1 — HTML files walked on disk in the last pass.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts-export", ts(optional))]
    pub last_reconcile_files: Option<u64>,
    /// R1 — synthetic `watch.delete` events emitted in the last pass
    /// (rows whose underlying file no longer exists).
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts-export", ts(optional))]
    pub last_reconcile_deletes: Option<u64>,
    /// R1 — duration of the last pass in milliseconds.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts-export", ts(optional))]
    pub last_reconcile_duration_ms: Option<u64>,
    /// R1 — configured reconciler interval in seconds. `0` means the
    /// reconciler is disabled for this kb. Returned alongside
    /// `last_reconcile_at` so callers can compute staleness.
    pub reconcile_secs: u64,
    /// GC-B2 — cumulative typed-decode skips (malformed lance batches
    /// dropped with a warn rather than aborting the read) since this kb's
    /// storage was opened. Nonzero means rows are being silently dropped
    /// from search/list results; see kb-core's `Storage::decode_skip_count`.
    pub decode_skips: u64,
}

pub async fn cross(State(state): State<Arc<KbHandles>>) -> Json<CrossStats> {
    // FF-E — fan out the per-kb stat reads concurrently (bounded), collecting
    // in BTreeMap order; the totals are summed from the returned rows. The
    // `last_reconcile` std::sync::Mutex is taken + dereferenced + dropped within
    // the snapshot expression, never held across an await (invariant 15).
    let mut futs: Vec<super::CorpusFut<'_, KbStats>> = Vec::new();
    for (name, ctx) in &state.kbs {
        futs.push(Box::pin(async move {
            let doc_count = ctx.storage.count_rows().await.unwrap_or(0);
            let errors = ctx.storage.list_open_errors().await.unwrap_or_default();
            let last = ctx
                .storage
                .last_run_for_source(ctx.source_slug.clone())
                .await
                .ok()
                .flatten();
            let last_index_at = last.and_then(|r| r.finished_at_unix);
            let reconcile_snapshot = ctx.last_reconcile.lock().ok().and_then(|guard| *guard);
            let decode_skips = ctx.storage.decode_skip_count().await.unwrap_or(0);
            KbStats {
                name: name.to_string(),
                doc_count,
                open_errors: errors.len() as u64,
                last_index_at,
                last_reconcile_at: reconcile_snapshot.map(|s| s.completed_at),
                last_reconcile_files: reconcile_snapshot.map(|s| s.files_walked),
                last_reconcile_deletes: reconcile_snapshot.map(|s| s.deletes_emitted),
                last_reconcile_duration_ms: reconcile_snapshot.map(|s| s.duration_ms),
                reconcile_secs: ctx.reconcile_secs,
                decode_skips,
            }
        }));
    }
    // PF-R1 — the operator-configurable `[server] fanout_cap` (default 8,
    // byte-identical to the old hardcoded `super::FANOUT_CAP`).
    let kbs = super::buffered_join(futs, state.fanout_cap).await;
    let total_docs: u64 = kbs.iter().map(|k| k.doc_count).sum();
    let total_open_errors: u64 = kbs.iter().map(|k| k.open_errors).sum();

    Json(CrossStats {
        daemon: DaemonStats {
            name: state.daemon_name.clone(),
            started_at: state.started_at.to_rfc3339(),
        },
        kbs,
        total_docs,
        total_open_errors,
    })
}

pub async fn per_kb(State(state): State<Arc<KbHandles>>, Path(kb): Path<String>) -> Response<Body> {
    let (kb_name, ctx) = match crate::routes::resolve_kb(&state, &kb) {
        Ok(v) => v,
        Err(resp) => return resp,
    };

    let doc_count = ctx.storage.count_rows().await.unwrap_or(0);
    let errors = ctx.storage.list_open_errors().await.unwrap_or_default();
    let last = ctx
        .storage
        .last_run_for_source(ctx.source_slug.clone())
        .await
        .ok()
        .flatten();
    let reconcile_snapshot = ctx.last_reconcile.lock().ok().and_then(|guard| *guard);
    let decode_skips = ctx.storage.decode_skip_count().await.unwrap_or(0);

    Json(KbStats {
        name: kb_name.to_string(),
        doc_count,
        open_errors: errors.len() as u64,
        last_index_at: last.and_then(|r| r.finished_at_unix),
        last_reconcile_at: reconcile_snapshot.map(|s| s.completed_at),
        last_reconcile_files: reconcile_snapshot.map(|s| s.files_walked),
        last_reconcile_deletes: reconcile_snapshot.map(|s| s.deletes_emitted),
        last_reconcile_duration_ms: reconcile_snapshot.map(|s| s.duration_ms),
        reconcile_secs: ctx.reconcile_secs,
        decode_skips,
    })
    .into_response()
}
