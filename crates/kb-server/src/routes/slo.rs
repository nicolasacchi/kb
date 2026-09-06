//! CT-F5 — corpus-health SLOs.
//!
//! * `GET  /api/kb/{kb}/slo` — the four indicators, computed now.
//! * `POST /api/kb/{kb}/slo/snapshot` — compute + append one run to the
//!   append-only log; returns the same report.
//! * `GET  /api/kb/{kb}/slo/snapshots[?limit=N]` — newest-first page over the
//!   log.
//!
//! **SURFACED, NEVER ENFORCED.** Nothing here changes any behaviour on a
//! missed target: no alert, no gate, no retry, no auto-repair, no scoring
//! consumer. An SLO miss is a number a human reads. The definitions live in
//! ONE place — `kb_core::slo` — and this module is only the plumbing that
//! feeds them raw counts.
//!
//! **Reads only existing tables.** Every input is a `COUNT`/`MAX`/projection
//! over `code_refs`, `sessions`, or the lance `kb_session` column, all of
//! which the corpus already maintains. The one exception is the CT-A3 recall
//! census, which needed V0039's three nullable `sessions` columns because the
//! counters were previously log-only (never queryable) — see that migration's
//! header.
//!
//! **The orphan indicator fans out.** `kb_session` is a cross-corpus hint
//! (invariant #11: a memory written during a session routinely lives in a
//! different corpus than the transcript), so "does this session id exist"
//! must be asked of EVERY kb on the daemon. That fan-out goes through
//! `routes::buffered_join(_, state.fanout_cap)` (PF-R1 — the operator-
//! configurable `[server] fanout_cap`, default 8, byte-identical to the old
//! hardcoded `FANOUT_CAP`) in BTreeMap submission order like every other
//! multi-kb read (invariant #28), each future folding a partial and
//! dropping its own errors so one sick corpus never 500s the report.

use crate::middleware::error_to_problem_json;
use crate::state::{KbContext, KbHandles};
use axum::{
    body::Body,
    extract::{Path, Query, State},
    http::Response,
    response::IntoResponse,
    Json,
};
use kb_core::slo::{SloInputs, SloReport};
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;
use std::sync::Arc;

/// Default + maximum page size for the snapshot log. Small on purpose: the
/// log is a trend read, not an export.
const SNAPSHOTS_DEFAULT_LIMIT: u32 = 100;
const SNAPSHOTS_MAX_LIMIT: u32 = 1_000;

#[derive(Debug, Deserialize, Default)]
pub struct SnapshotsQuery {
    pub limit: Option<u32>,
}

/// `POST …/slo/snapshot`'s body: the report that was appended, plus the run's
/// identity (`taken_at_unix`, shared by all of its rows) and how many rows
/// landed. The report is echoed so a caller never has to issue a second
/// request to see what it just recorded — and so what it renders is provably
/// the same reading that was stored.
#[derive(Debug, Serialize)]
pub struct SnapshotResponse {
    pub taken_at_unix: i64,
    pub appended: usize,
    pub report: SloReport,
}

#[derive(Debug, Serialize)]
pub struct SnapshotsResponse {
    pub kb: String,
    pub rows: Vec<kb_core::storage::sqlite::SloSnapshotRow>,
}

/// `GET /api/kb/{kb}/slo`.
pub async fn get(State(state): State<Arc<KbHandles>>, Path(kb): Path<String>) -> Response<Body> {
    let (kb_name, ctx) = match crate::routes::resolve_kb(&state, &kb) {
        Ok(v) => v,
        Err(resp) => return resp,
    };
    let report = match compute(&state, kb_name.as_str(), ctx).await {
        Ok(r) => r,
        Err(resp) => return resp,
    };
    Json(report).into_response()
}

/// `POST /api/kb/{kb}/slo/snapshot`.
///
/// Computes exactly what `GET …/slo` would return, then appends it. There is
/// deliberately no "only if changed" skip (unlike `atlas_snapshots`'
/// `coord_hash` dedup): a flat-lining indicator is itself the signal, so every
/// run must land. And there is no schedule — the daemon never snapshots on its
/// own. A trend log that fills itself would quietly become a retention
/// question; an operator (or their cron) decides when a reading is worth
/// keeping.
pub async fn snapshot(
    State(state): State<Arc<KbHandles>>,
    Path(kb): Path<String>,
) -> Response<Body> {
    let (kb_name, ctx) = match crate::routes::resolve_kb(&state, &kb) {
        Ok(v) => v,
        Err(resp) => return resp,
    };
    let report = match compute(&state, kb_name.as_str(), ctx).await {
        Ok(r) => r,
        Err(resp) => return resp,
    };
    let taken_at_unix = report.computed_at_unix;
    let appended = match ctx
        .storage
        .slo_snapshot_append(taken_at_unix, report.indicators.clone())
        .await
    {
        Ok(n) => n,
        Err(e) => return error_to_problem_json(&e),
    };
    Json(SnapshotResponse {
        taken_at_unix,
        appended,
        report,
    })
    .into_response()
}

/// `GET /api/kb/{kb}/slo/snapshots[?limit=N]`.
pub async fn snapshots(
    State(state): State<Arc<KbHandles>>,
    Path(kb): Path<String>,
    Query(params): Query<SnapshotsQuery>,
) -> Response<Body> {
    let (kb_name, ctx) = match crate::routes::resolve_kb(&state, &kb) {
        Ok(v) => v,
        Err(resp) => return resp,
    };
    let limit = params
        .limit
        .unwrap_or(SNAPSHOTS_DEFAULT_LIMIT)
        .clamp(1, SNAPSHOTS_MAX_LIMIT);
    match ctx.storage.slo_snapshots_list(limit).await {
        Ok(rows) => Json(SnapshotsResponse {
            kb: kb_name.to_string(),
            rows,
        })
        .into_response(),
        Err(e) => error_to_problem_json(&e),
    }
}

/// Gather the raw counts and hand them to `kb_core::slo::build`.
///
/// Errors here DO fail the request (unlike the per-corpus fan-out below,
/// whose partial failures are folded away): these are this kb's own reads, and
/// a report that silently degraded three indicators to `unknown` because
/// sqlite was unhappy would be indistinguishable from a genuinely unmeasurable
/// corpus — exactly the confusion the `unknown` status exists to avoid.
async fn compute(
    state: &Arc<KbHandles>,
    kb_name: &str,
    ctx: &KbContext,
) -> Result<SloReport, Response<Body>> {
    let (coderef_total, coderef_path_shaped) = ctx
        .storage
        .code_ref_shape_counts()
        .await
        .map_err(|e| error_to_problem_json(&e))?;

    let session_counts = ctx
        .storage
        .kb_session_doc_counts()
        .await
        .map_err(|e| error_to_problem_json(&e))?;
    let (docs_with_kb_session, orphan_kb_session_docs) =
        orphan_counts(state, &session_counts).await;

    let (recall_marker_parsed, recall_fallback_parsed, recall_failed, recall_censused_captures) =
        ctx.storage
            .sessions_recall_census_totals()
            .await
            .map_err(|e| error_to_problem_json(&e))?;

    let newest_session_started_at = ctx
        .storage
        .sessions_newest_started_at()
        .await
        .map_err(|e| error_to_problem_json(&e))?;

    let inputs = SloInputs {
        coderef_total,
        coderef_path_shaped,
        docs_with_kb_session,
        orphan_kb_session_docs,
        recall_marker_parsed,
        recall_fallback_parsed,
        recall_failed,
        recall_censused_captures,
        newest_session_started_at,
    };
    Ok(kb_core::slo::build(
        kb_name,
        &inputs,
        &ctx.slo_targets,
        chrono::Utc::now().timestamp(),
    ))
}

/// `(docs_carrying_a_session, docs_whose_session_resolves_nowhere)`.
///
/// The membership question is asked of EVERY kb on this daemon (invariant #11
/// again: the transcript's `sessions` row lives wherever the transcripts
/// corpus is mounted, which is usually not the corpus holding the doc). The
/// fan-out uses `buffered_join` in BTreeMap submission order with the
/// operator-configurable `state.fanout_cap` (PF-R1; invariant #28), and
/// each future folds its own error away —
/// a corpus that fails to answer simply contributes no "present" ids.
///
/// That drop-on-error skew is deliberately CONSERVATIVE in the wrong
/// direction (a failing corpus can only INFLATE the orphan count, never hide
/// one) and is the right trade for a surfaced-never-enforced number: an
/// inflated count invites a look, a suppressed one would be a false all-clear.
async fn orphan_counts(
    state: &Arc<KbHandles>,
    session_counts: &std::collections::HashMap<String, u64>,
) -> (u64, u64) {
    let docs_with_kb_session: u64 = session_counts.values().sum();
    if session_counts.is_empty() {
        return (0, 0);
    }
    // Sorted so the id list handed to every corpus is deterministic (the
    // `IN (…)` bind order shows up in query plans and in any future log).
    let ids: Vec<String> = session_counts
        .keys()
        .cloned()
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect();

    let mut futs: Vec<crate::routes::CorpusFut<'_, Vec<String>>> = Vec::new();
    for ctx in state.kbs.values() {
        let ids = ids.clone();
        futs.push(Box::pin(async move {
            ctx.storage
                .session_ids_present(ids)
                .await
                .unwrap_or_default()
        }));
    }
    // PF-R1 — the operator-configurable `[server] fanout_cap` (default 8,
    // byte-identical to the old hardcoded `crate::routes::FANOUT_CAP`).
    let known: BTreeSet<String> = crate::routes::buffered_join(futs, state.fanout_cap)
        .await
        .into_iter()
        .flatten()
        .collect();

    let orphans = session_counts
        .iter()
        .filter(|(sid, _)| !known.contains(*sid))
        .map(|(_, n)| *n)
        .sum();
    (docs_with_kb_session, orphans)
}
