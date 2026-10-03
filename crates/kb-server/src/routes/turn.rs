//! `GET /api/turn` — the turn block the recall hook builds from separate
//! client calls, composed in-process.
//!
//! Lanes are the functions `/api/memory/recall` and `/api/context` already
//! use (`recall_compose`, `context::get`). A lane that errors, times out,
//! or returns a non-success response is named in `degraded` and does not
//! fail the request. Ranking is whatever `recall_compose` returns; this
//! route does not re-sort or re-score.
//!
//! `project` / `visible_to` narrow the recall lane exactly as `kb recall`'s
//! auto scope does (`scope=all` + both fields on `RecallParams`); the CLI
//! derives them from its cwd with no network call and this route checks the
//! project against its own `[kb.*]` config (a sync map lookup, no storage
//! fan-out). An unknown project fails open to unscoped recall. `lanes=`
//! (csv of `recall`, `context`) skips lanes the caller does not want.
//!
//! `head_seq` is omitted. The slate open helper is not callable from this
//! crate without editing another file: `load_meta` in
//! `crates/kb-server/src/routes/slates.rs` is private, and `slates::get`
//! needs a slug this query does not carry (cwd → slug lives in the CLI).

use crate::middleware::error_to_problem_json;
use crate::routes::context::{
    deadline_at, degraded_of, remaining_ms, within_deadline, ContextParams, DegradedLane,
    QueryErrorClass,
};
use crate::routes::memory::{self, Params as RecallParams, RecallResult};
use crate::state::KbHandles;
use axum::body::to_bytes;
use axum::extract::{Extension, Query, State};
use axum::http::{header, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::Json;
use kb_core::storage::sqlite::ServedRecallRow;
use serde::{Deserialize, Serialize};
use std::sync::Arc;

/// Hook default (`kb recall … --limit 5`). Not a re-rank — the composer
/// already truncates to `limit` after its own ordering.
const RECALL_LIMIT: usize = 5;
/// v2 depth by served position, matching `kb-recall.sh`: hits 1–2 keep 320
/// summary chars, hit 3 keeps 200, hits 4+ are title-only.
const SUMMARY_CAP_TOP: usize = 320;
const SUMMARY_CAP_MID: usize = 200;

#[derive(Debug, Deserialize, Default)]
pub struct TurnParams {
    /// Task text. `prompt` is the hook payload's name for the same field.
    #[serde(default)]
    pub q: String,
    #[serde(default)]
    pub prompt: String,
    pub session: Option<String>,
    pub cwd: Option<String>,
    /// Shared budget for both lanes. An arm that misses it is dropped and
    /// named `error_class: timeout`; absent means no cap.
    pub deadline_ms: Option<u64>,
    /// Memory corpus name to narrow recall to (`memory-<slug>`). Checked
    /// against the daemon's configured kbs; unknown ⇒ ignored.
    pub project: Option<String>,
    /// csv of slugs/corpus names always visible to this project's recall.
    /// Only honoured together with a confirmed `project`.
    pub visible_to: Option<String>,
    /// csv subset of `recall,context`. Absent/blank ⇒ both.
    pub lanes: Option<String>,
}

/// Which lanes a request wants. `Err` carries the offending token.
fn parse_lanes(raw: Option<&str>) -> Result<(bool, bool), String> {
    let Some(raw) = raw.map(str::trim).filter(|s| !s.is_empty()) else {
        return Ok((true, true));
    };
    let (mut recall, mut context) = (false, false);
    for tok in raw.split(',').map(str::trim).filter(|t| !t.is_empty()) {
        match tok {
            "recall" => recall = true,
            "context" => context = true,
            other => return Err(other.to_string()),
        }
    }
    Ok((recall, context))
}

/// Project/visible_to as they go onto `RecallParams`: the project only when
/// it names a configured kb (config lookup, no storage), and `visible_to`
/// only alongside it.
fn confirmed_scope(
    state: &KbHandles,
    project: Option<&str>,
    visible_to: Option<&str>,
) -> (Option<String>, Option<String>) {
    let known = |p: &str| {
        kb_core::types::KbName::new(p)
            .ok()
            .is_some_and(|n| state.kbs.contains_key(&n))
    };
    scope_of(known, project, visible_to)
}

/// Pure core of [`confirmed_scope`]: `known` answers "is this a configured kb".
fn scope_of(
    known: impl Fn(&str) -> bool,
    project: Option<&str>,
    visible_to: Option<&str>,
) -> (Option<String>, Option<String>) {
    let Some(project) = project.map(str::trim).filter(|p| !p.is_empty()) else {
        return (None, None);
    };
    if !known(project) {
        return (None, None);
    }
    (
        Some(project.to_string()),
        visible_to
            .map(str::trim)
            .filter(|v| !v.is_empty())
            .map(str::to_string),
    )
}

#[derive(Debug, Serialize)]
pub struct RecalledHit {
    pub kb: String,
    pub id: String,
    pub pos: u32,
    pub title: String,
}

#[derive(Debug, Serialize)]
pub struct TurnResponse {
    /// The additionalContext string, recall markers included.
    pub text: String,
    pub recalled: Vec<RecalledHit>,
    /// Swallowed lane failures. Absent when empty.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub degraded: Vec<DegradedLane>,
}

/// `GET /api/turn?q=&prompt=&session=&cwd=&deadline_ms=&project=&visible_to=&lanes=`
pub async fn get(
    State(state): State<Arc<KbHandles>>,
    Extension(identity): Extension<crate::middleware::Identity>,
    Query(params): Query<TurnParams>,
) -> Response {
    let q = non_empty(&params.q).unwrap_or_else(|| params.prompt.clone());
    let q = q.trim().to_string();
    if q.is_empty() {
        return error_to_problem_json(&kb_core::Error::BadRequest(
            "q or prompt is required".into(),
        ));
    }
    let (want_recall, want_context) = match parse_lanes(params.lanes.as_deref()) {
        Ok(l) => l,
        Err(bad) => {
            return error_to_problem_json(&kb_core::Error::BadRequest(format!(
                "unknown lane {bad:?}; expected a csv of: recall, context"
            )));
        }
    };
    let (project, visible_to) = confirmed_scope(
        &state,
        params.project.as_deref(),
        params.visible_to.as_deref(),
    );
    let cwd = params.cwd.as_deref().and_then(non_empty);
    let session = params.session.as_deref().and_then(non_empty);
    let deadline = deadline_at(params.deadline_ms);
    let mut degraded: Vec<DegradedLane> = Vec::new();

    // Sequential on purpose: both lanes embed the same text, and the context
    // composer already refuses to run its embed-bearing arms concurrently.
    let hits = if !want_recall {
        Vec::new()
    } else {
        match run_recall(
            &state,
            identity.clone(),
            &q,
            project,
            visible_to,
            remaining_ms(deadline),
            deadline,
        )
        .await
        {
            Ok(recall) => {
                extend_degraded(&mut degraded, recall.degraded);
                recall.hits
            }
            Err(class) => {
                push_degraded(&mut degraded, degraded_of("turn", "recall", class));
                Vec::new()
            }
        }
    };

    let scent = if !want_context {
        String::new()
    } else {
        match run_context(
            &state,
            identity,
            &q,
            cwd,
            session.clone(),
            remaining_ms(deadline),
            deadline,
        )
        .await
        {
            Ok((scent, lanes)) => {
                extend_degraded(&mut degraded, lanes);
                scent
            }
            Err(class) => {
                push_degraded(&mut degraded, degraded_of("turn", "context", class));
                String::new()
            }
        }
    };
    // Same served-recall rows as GET /api/memory/recall?session=.
    // routes::memory::record_served_recalls is private; this is the
    // smallest append. A write failure must not 500 the turn or drop
    // degraded[].
    if let Some(session_id) = session.as_deref() {
        record_served_turn(&state, session_id, &hits);
    }

    let recalled = recalled_of(&hits);
    let text = compose_text(&hits, &scent);
    (
        [(header::CACHE_CONTROL, "no-store")],
        Json(TurnResponse {
            text,
            recalled,
            degraded,
        }),
    )
        .into_response()
}

fn non_empty(s: &str) -> Option<String> {
    let s = s.trim();
    if s.is_empty() {
        None
    } else {
        Some(s.to_string())
    }
}

async fn run_recall(
    state: &Arc<KbHandles>,
    identity: crate::middleware::Identity,
    q: &str,
    project: Option<String>,
    visible_to: Option<String>,
    deadline_ms: Option<u64>,
    deadline: Option<std::time::Instant>,
) -> Result<memory::RecallResponse, QueryErrorClass> {
    if matches!(deadline_ms, Some(0)) {
        return Err(QueryErrorClass::Timeout);
    }
    let composed = within_deadline(
        deadline,
        memory::recall_compose(
            Arc::clone(state),
            identity,
            RecallParams {
                q: q.to_string(),
                scope: "all".to_string(),
                // The caller's already-derived scope (see `confirmed_scope`);
                // absent keeps the served order equal to `recall_compose`'s
                // own ranking — this route does not re-rank.
                project,
                limit: Some(RECALL_LIMIT),
                for_kb: None,
                visible_to,
                no_floor: false,
                with_weekly: false,
                deadline_ms,
            },
        ),
    )
    .await;
    match composed {
        Err(()) => Err(QueryErrorClass::Timeout),
        Ok(Ok(recall)) => Ok(recall),
        Ok(Err(resp)) => Err(class_of_failure(resp).await),
    }
}

async fn run_context(
    state: &Arc<KbHandles>,
    identity: crate::middleware::Identity,
    q: &str,
    cwd: Option<String>,
    session: Option<String>,
    deadline_ms: Option<u64>,
    deadline: Option<std::time::Instant>,
) -> Result<(String, Vec<DegradedLane>), QueryErrorClass> {
    if matches!(deadline_ms, Some(0)) {
        return Err(QueryErrorClass::Timeout);
    }
    let resp = match within_deadline(
        deadline,
        crate::routes::context::get(
            State(Arc::clone(state)),
            Extension(identity),
            Query(ContextParams {
                q: q.to_string(),
                cwd,
                session,
                deadline_ms,
                ..ContextParams::default()
            }),
        ),
    )
    .await
    {
        Err(()) => return Err(QueryErrorClass::Timeout),
        Ok(resp) => resp,
    };
    let status = resp.status();
    let bytes = match to_bytes(resp.into_body(), 2 * 1024 * 1024).await {
        Ok(b) => b,
        Err(_) => return Err(QueryErrorClass::Other),
    };
    if !status.is_success() {
        return Err(class_of_status(status, &bytes));
    }
    let value: serde_json::Value = match serde_json::from_slice(&bytes) {
        Ok(v) => v,
        Err(_) => return Err(QueryErrorClass::Other),
    };
    let scent = value
        .get("scent")
        .and_then(|s| s.as_str())
        .unwrap_or("")
        .to_string();
    Ok((scent, parse_degraded(value.get("degraded"))))
}

fn parse_degraded(value: Option<&serde_json::Value>) -> Vec<DegradedLane> {
    let Some(arr) = value.and_then(|v| v.as_array()) else {
        return Vec::new();
    };
    arr.iter()
        .filter_map(|item| {
            let kb = item.get("kb")?.as_str()?.to_string();
            let lane = item.get("lane")?.as_str()?.to_string();
            let error_class = match item.get("error_class").and_then(|c| c.as_str()) {
                Some("index_fragment") => QueryErrorClass::IndexFragment,
                Some("timeout") => QueryErrorClass::Timeout,
                Some("storage") => QueryErrorClass::Storage,
                Some("embed") => QueryErrorClass::Embed,
                Some("other") => QueryErrorClass::Other,
                _ => return None,
            };
            Some(DegradedLane {
                kb,
                lane,
                error_class,
            })
        })
        .collect()
}

async fn class_of_failure(resp: Response) -> QueryErrorClass {
    let status = resp.status();
    let bytes = to_bytes(resp.into_body(), 4096).await.unwrap_or_default();
    class_of_status(status, &bytes)
}

fn class_of_status(status: StatusCode, bytes: &[u8]) -> QueryErrorClass {
    if status == StatusCode::REQUEST_TIMEOUT || status == StatusCode::GATEWAY_TIMEOUT {
        return QueryErrorClass::Timeout;
    }
    let text = String::from_utf8_lossy(bytes);
    crate::routes::context::classify_query_error(&text)
}

fn extend_degraded(out: &mut Vec<DegradedLane>, lanes: Vec<DegradedLane>) {
    for lane in lanes {
        push_degraded(out, lane);
    }
}

fn push_degraded(out: &mut Vec<DegradedLane>, lane: DegradedLane) {
    if !out.iter().any(|d| d == &lane) {
        out.push(lane);
    }
}

fn recalled_of(hits: &[RecallResult]) -> Vec<RecalledHit> {
    hits.iter()
        .enumerate()
        .map(|(i, h)| RecalledHit {
            kb: h.kb.clone(),
            id: h.id.clone(),
            pos: u32::try_from(i).unwrap_or(u32::MAX).saturating_add(1),
            title: h.title.clone(),
        })
        .collect()
}

/// Hook v2 block (the default layout) plus the scent line the hook appends.
/// Markers carry the served position, which is also `recalled[].pos`.
fn compose_text(hits: &[RecallResult], scent: &str) -> String {
    let mut block = render_recall(hits);
    let scent_line = scent_line(scent);
    if !block.is_empty() && !scent_line.is_empty() {
        block.push_str("\n\n");
        block.push_str(&scent_line);
    } else if block.is_empty() {
        block = scent_line;
    }
    block
}

fn render_recall(hits: &[RecallResult]) -> String {
    if hits.is_empty() {
        return String::new();
    }
    let mut lines = Vec::with_capacity(hits.len());
    for (i, hit) in hits.iter().enumerate() {
        let pos = u32::try_from(i).unwrap_or(u32::MAX).saturating_add(1);
        lines.push(v2_line(hit, pos));
    }
    format!(
        "Relevant memories from kb (recall — these persist across sessions):\n{}",
        lines.join("\n")
    )
}

fn v2_line(hit: &RecallResult, pos: u32) -> String {
    let cap = if pos <= 2 {
        SUMMARY_CAP_TOP
    } else if pos == 3 {
        SUMMARY_CAP_MID
    } else {
        0
    };
    let mut pfx = String::new();
    if hit.flagged {
        pfx.push_str("⚠ disputed: ");
    }
    if hit.warns {
        pfx.push_str("✗ didn't work: ");
    }
    let drift = if hit.drift_open > 0 {
        format!(" [⚠ {} drift-flagged citation(s)]", hit.drift_open)
    } else {
        String::new()
    };
    let summary = summary_line(hit.summary.as_deref().unwrap_or(""), cap);
    format!(
        "- {pfx}{}  [{}]{drift}{summary}\n<!--kb-recall/1 kb={} id={} pos={pos}-->",
        hit.title, hit.kb, hit.kb, hit.id
    )
}

fn summary_line(summary: &str, cap: usize) -> String {
    if cap == 0 || summary.is_empty() {
        return String::new();
    }
    // One line, always: the capture parser (`derive_memory_recalls`) reads
    // the injected block line by line, so a newline inside a summary would
    // split one hit into several lines.
    let clipped: String = summary
        .chars()
        .map(|c| if c == '\n' || c == '\r' { ' ' } else { c })
        .take(cap)
        .collect();
    format!("\n    ↳ {clipped}")
}

/// The hook's turn-1 scent wrapper. `"no prior context"` is the route's
/// honest empty answer and is not injected.
fn scent_line(scent: &str) -> String {
    let scent = scent.trim();
    if scent.is_empty() || scent == "no prior context" {
        return String::new();
    }
    format!(
        "kb has prior context for this task — {scent}.\n\
Counts only (nothing episodic is auto-injected). Run `kb context \"<your task>\"` to pull the pack: prior-session pointers, open comments on matching artifacts, and the code paths those artifacts cite."
    )
}

/// Same caps as `routes::memory`'s serve-time ledger. Private there.
const TURN_SERVED_WRITE_CAP: usize = 50;
const TURN_SERVED_TITLE_CAP: usize = 240;
const TURN_SERVED_SESSION_CAP: usize = 128;

/// Record the hits this turn returned. Never fails the response: a bad
/// id, a missing sessions corpus, or a write error is logged and dropped.
/// Fire-and-forget (the same `tokio::spawn` `GET /api/memory/recall` uses):
/// the append queues behind the sessions corpus's write lane, which must
/// not sit on the turn's request path or outside its deadline.
/// INSERT only — never `memory_recalls_replace`.
fn record_served_turn(state: &KbHandles, session_id: &str, hits: &[RecallResult]) {
    let Some(session_id) = accept_turn_session(session_id) else {
        return;
    };
    let rows = served_turn_rows(hits);
    if rows.is_empty() {
        return;
    }
    let Some(storage) = sessions_storage(state) else {
        tracing::debug!(
            session_id,
            n = rows.len(),
            "served recall ledger skipped: no sessions corpus"
        );
        return;
    };
    tokio::spawn(async move {
        if let Err(e) = storage
            .memory_recalls_append(session_id.clone(), rows)
            .await
        {
            tracing::debug!(
                session_id,
                error = %e,
                "served recall ledger not written"
            );
        }
    });
}

fn accept_turn_session(raw: &str) -> Option<String> {
    let id = raw.trim();
    if id.is_empty() || id.chars().count() > TURN_SERVED_SESSION_CAP {
        return None;
    }
    if id.chars().any(|c| c.is_control() || c.is_whitespace()) {
        return None;
    }
    Some(id.to_string())
}

fn served_turn_rows(hits: &[RecallResult]) -> Vec<ServedRecallRow> {
    let served_at = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    hits.iter()
        .take(TURN_SERVED_WRITE_CAP)
        .enumerate()
        .map(|(i, hit)| {
            let title_chars = hit.title.chars().count();
            let summary_chars = hit
                .summary
                .as_deref()
                .map(|s| s.chars().count())
                .unwrap_or(0);
            ServedRecallRow {
                memory_kb: hit.kb.clone(),
                memory_id: hit.id.clone(),
                pos: (i as u32).saturating_add(1),
                title: clip_chars(&hit.title, TURN_SERVED_TITLE_CAP),
                injected_chars: u32::try_from(title_chars.saturating_add(summary_chars))
                    .unwrap_or(u32::MAX),
                served_at,
            }
        })
        .collect()
}

fn clip_chars(s: &str, cap: usize) -> String {
    if s.chars().count() <= cap {
        s.to_string()
    } else {
        s.chars().take(cap).collect()
    }
}

/// Sessions corpus: `default_search_category = "memory-session"`, else a
/// kb named `sessions`. Same lookup as the recall route.
fn sessions_storage(state: &KbHandles) -> Option<kb_core::storage::StorageHandle> {
    let by_category = state
        .kbs
        .iter()
        .find(|(_, ctx)| ctx.default_search_category.as_deref() == Some("memory-session"));
    let (_, ctx) = by_category.or_else(|| {
        state
            .kbs
            .iter()
            .find(|(name, _)| name.as_str() == "sessions")
    })?;
    Some(ctx.storage.clone())
}

#[cfg(test)]
mod summary_line_tests {
    use super::summary_line;

    /// A memory summary with a blank line in it must still render as ONE
    /// `↳` line: the capture parser walks the injected block line by line.
    #[test]
    fn summary_line_flattens_newlines() {
        let out = summary_line("Fix:\n\nuse X\r\nnow", 160);
        assert_eq!(out, "\n    ↳ Fix:  use X  now");
        assert!(!out[1..].contains('\n'));
        assert!(!out.contains('\r'));
    }
}

#[cfg(test)]
mod scope_tests {
    use super::scope_of;

    fn known(p: &str) -> bool {
        p == "memory-a"
    }

    #[test]
    fn known_project_passes_project_and_visible_to() {
        assert_eq!(
            scope_of(known, Some(" memory-a "), Some("a, memory-a")),
            (Some("memory-a".into()), Some("a, memory-a".into()))
        );
        assert_eq!(
            scope_of(known, Some("memory-a"), Some("  ")),
            (Some("memory-a".into()), None)
        );
    }

    #[test]
    fn unknown_project_yields_no_scope_at_all() {
        assert_eq!(scope_of(known, Some("memory-z"), Some("a")), (None, None));
    }

    #[test]
    fn visible_to_is_dropped_without_a_project() {
        assert_eq!(scope_of(known, None, Some("a")), (None, None));
        assert_eq!(scope_of(known, Some(""), Some("a")), (None, None));
    }
}

#[cfg(test)]
mod lanes_tests {
    use super::parse_lanes;

    #[test]
    fn lanes_default_to_both_and_parse_subsets() {
        assert_eq!(parse_lanes(None), Ok((true, true)));
        assert_eq!(parse_lanes(Some("  ")), Ok((true, true)));
        assert_eq!(parse_lanes(Some("recall")), Ok((true, false)));
        assert_eq!(parse_lanes(Some("context, recall")), Ok((true, true)));
        assert_eq!(parse_lanes(Some("context")), Ok((false, true)));
    }

    /// `slate` is not a lane of this route yet; naming it is an error, not a
    /// silent no-op that would let a caller believe it got a slate head.
    #[test]
    fn unknown_lane_is_refused() {
        assert_eq!(parse_lanes(Some("recall,slate")), Err("slate".to_string()));
    }
}

#[cfg(test)]
mod hook_parity_tests {
    use super::{compose_text, RecallResult};

    fn hit(
        kb: &str,
        id: &str,
        title: &str,
        summary: Option<String>,
        flagged: bool,
        warns: bool,
        drift_open: u32,
    ) -> RecallResult {
        RecallResult {
            id: id.to_string(),
            kb: kb.to_string(),
            title: title.to_string(),
            path: format!("/{id}.html"),
            source_relative: format!("{id}.html"),
            score: 1.0,
            salience: 0.5,
            pinned: false,
            session_id: None,
            summary,
            memory_type: None,
            source: None,
            author: None,
            source_kb: None,
            source_artifact: None,
            source_anchor: None,
            global: false,
            linked_kbs: Vec::new(),
            read_pct: None,
            last_read_at: None,
            stopped_at: None,
            rank: None,
            rel: None,
            decay: None,
            age_days: None,
            recall_count: 0,
            last_recalled_at: None,
            recall_used_count: 0,
            relevance_factor: None,
            stability: None,
            decay_k: None,
            recall_weekly: Vec::new(),
            flagged,
            warns,
            code_hints: Vec::new(),
            code_hints_total: 0,
            drift_open,
        }
    }

    /// The route's recall block must be the bytes `kb-recall.sh` (layout v2,
    /// the default) injects for the same pack. The fixture was captured from
    /// the hook itself; `test-recall-layout.sh` pins the hook side of it.
    #[test]
    fn compose_text_matches_the_v2_hook_golden() {
        let hits = vec![
        hit(
            "kb",
            "a1b2c3d4e5f6",
            "demo-repo build cache setup",
            Some("installed 2026-08-01; measured a modest speedup of roughly 15% (not the hoped-for 60%) because the build host is disk-bound not compute-bound; the parallel-jobs convention only helps once the scheduler is tuned, and the stale-lock gotcha bites when a build is killed mid-link rather than left to finish naturally.".to_string()),
            false,
            false,
            0,
        ),
        hit(
            "main",
            "0f0f0f0f0f0f",
            "Wrong port for the staging service",
            Some("the staging service binds 127.0.0.1:9000 in the demo env and a spare port everywhere else; never curl 9000 from a test, and never bind it either — the demo container owns it and a bind fails closed, not shared.".to_string()),
            true,
            false,
            1,
        ),
        hit(
            "kb",
            "111111111111",
            "demo-repo fixture builds are IO-bound, not CPU-bound",
            Some("the demo-repo build tree and its fixture corpus share one disk, so a parallel build starves the test run; tuned by pruning a background index job, switching the disk scheduler and lowering swap use.".to_string()),
            false,
            false,
            0,
        ),
        hit(
            "kb",
            "222222222222",
            "demo-repo CI workflow facts",
            Some("sign-off needs a signed commit; the lint step runs before tests so a lint failure masks test failures; the main workflow only triggers on PRs to main, so stacked PRs run signature checks only.".to_string()),
            false,
            true,
            0,
        ),
        hit(
            "main",
            "333333333333",
            "Demo service file-descriptor ceiling",
            Some("a long soak run exhausted the demo service's file descriptors and every request failed with too many open files; the fix is raising the descriptor limit and restarting the service.".to_string()),
            false,
            false,
            0,
        ),
        ];
        let golden =
            include_str!("../../../../plugins/kb-memory/hooks/tests/fixtures/recall-layout-v2.txt");
        assert_eq!(
            compose_text(&hits, "no prior context"),
            golden.trim_end_matches('\n')
        );
    }
}
