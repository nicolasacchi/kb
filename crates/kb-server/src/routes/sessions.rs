//! v0.14 S3 — `/api/sessions/*` routes. The SPA's /sessions view is
//! built on these:
//!
//! - `GET  /api/sessions`                       cross-kb list, newest first
//! - `GET  /api/sessions/{session_id}`          single + memory id list
//! - `GET  /api/sessions/{session_id}/memories` ranked memory hits
//!
//! S4 `/touches` (transcript scan + LRU) lives here too. All routes fan out
//! across `state.kbs` in BTreeMap order; per-kb failures are logged
//! and skipped (one corrupted kb must not poison the cross-kb list).
//! The enrichment metadata lives in each kb's V0008 `sessions` sqlite
//! table; the per-row title is projected from lance at request time so
//! the response stays in sync with the live indexer.
//!
//! `memory_count` is computed per session via a cheap
//! `count_docs_with_kb_session` filter scan across every kb. Memory
//! corpora typically hold a few hundred rows, so the scan is fast
//! enough not to need a cache for v0.14.

use crate::middleware::error_to_problem_json;
use crate::state::KbHandles;
use axum::{
    extract::{ConnectInfo, Path, Query, State},
    http::{header, HeaderMap, HeaderValue, StatusCode},
    response::{IntoResponse, Response},
    Json,
};
// LSC-2 — the two-axis live-session vocabulary (LSC-1, reused verbatim: the
// beat route/registry never re-derive state logic).
use kb_core::sessions::live::{derive_state, Holder, LivePolicy, LiveState, StateSource};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::Arc;

/// T5 — default per-page cap on /api/sessions. Smaller than the
/// initial 200 because the SPA is now cursor-paginated; matches the
/// `kb sessions list` CLI default. Hard cap (`MAX_LIMIT`) prevents
/// an oversized `?limit=…` from blowing out the response.
///
/// W0.6 — bumped 500 → 1000: `kb sessions list --limit N`/`search` (the CLI
/// wrapper in `commands/sessions.rs`) never forwarded `--limit` to the
/// server, so every call silently capped at the OLD default-only response —
/// a `--limit 900` request returned 50 rows. Fixed to forward `?limit=`; this
/// bump gives the forwarded value real headroom.
const DEFAULT_LIMIT: u32 = 50;
const MAX_LIMIT: u32 = 1000;
/// Per-session memory-list cap. 100 is well past what any single
/// session is expected to produce; the SPA will paginate if needed.
const PER_SESSION_MEMORY_LIMIT: u32 = 100;

/// W0.6 — the shortest sha prefix `/api/sessions/by-commit` accepts. Below
/// this, a "prefix" is common enough across an active repo's history that a
/// match is more noise than signal; git's own abbreviation floor is similar.
const BY_COMMIT_MIN_PREFIX: usize = 7;
/// W0.6 — `/api/sessions/commit-map` pagination. Default mirrors the task's
/// "keep it flat and cheap" framing; `MAX` is generous since this is an
/// internal bulk feed (kb-code's wave-3 join precomputation), not a UI page.
const COMMIT_MAP_DEFAULT_LIMIT: u32 = 500;
const COMMIT_MAP_MAX_LIMIT: u32 = 5000;
/// W0.6 — federated `commit-map` pagination over-fetches each kb up to
/// `offset + limit` rows (mirrors the `sessions::list` overfetch-then-merge
/// pattern, invariant #28), capped here so a client-supplied huge `offset`
/// can't force an unbounded per-kb scan.
const COMMIT_MAP_MAX_PER_KB_FETCH: u32 = 50_000;

/// FF-D — the located session row in `touches`: `(kb, artifact_id,
/// transcript_path, mtime_unix)`. Named so the fan-out future type stays under
/// `clippy::type_complexity`.
type LocatedSession = (String, String, std::path::PathBuf, i64);

#[cfg_attr(
    feature = "ts-export",
    derive(ts_rs::TS),
    ts(export, rename = "SessionRow")
)]
#[derive(Debug, Serialize)]
pub struct SessionOut {
    pub id: String,
    pub kb: String,
    pub artifact_id: String,
    pub session_id: String,
    pub started_at: i64,
    pub ended_at: i64,
    pub duration_ms: i64,
    pub message_count: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts-export", ts(optional))]
    pub first_user_prompt: Option<String>,
    pub memory_count: u64,
    pub source_relative: String,
    /// aiTitle from the persisted V0017 `sessions.title` column. `None` when
    /// the transcript had no aiTitle (old sessions / un-reindexed rows).
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts-export", ts(optional))]
    pub title: Option<String>,
    /// A2/A3 — the row's readable name, server-computed so every surface
    /// (SPA, CLI) agrees: title → first_user_prompt → short session id.
    /// Never null.
    pub display_name: String,
    /// A1 — the session's modal working directory (full path), or `None`.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts-export", ts(optional))]
    pub cwd: Option<String>,
    /// A1 — the folder label (basename of `cwd`) for the facet/badge.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts-export", ts(optional))]
    pub folder: Option<String>,
    /// First git branch the session ran on.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts-export", ts(optional))]
    pub git_branch: Option<String>,
    /// Count of files the session read.
    pub files_read_count: u32,
    /// Count of distinct files the session edited/wrote.
    pub files_edited_count: u32,
    /// S9 — total tokens (input + output).
    pub token_total: u64,
    /// S9 — number of tool calls.
    pub tool_calls: u32,
    /// S9 — the model the session ran on.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts-export", ts(optional))]
    pub model: Option<String>,
    /// S9 — count of detected error tool-results.
    pub error_count: u32,
    /// W0.2 — count of Agent/Task delegations that came back with real stats
    /// (a synchronous completion). See `subagent_launched_unstatted` for
    /// delegations that produced no numbers (the async/background default).
    pub subagent_count: u32,
    /// W0.2 — summed tokens over every completed-with-stats subagent.
    pub subagent_tokens: u64,
    /// W0.2 — summed tool-call counts over every completed-with-stats
    /// subagent.
    pub subagent_tool_calls: u32,
    /// W0.2 — summed edit-operation counts over every completed-with-stats
    /// subagent (detected, not ground truth).
    pub subagent_files_edited: u32,
    /// W0.2 — count of Agent/Task delegations with NO stats attached (an
    /// async stub, or any other agentId-bearing result missing
    /// `totalTokens`). Kept separate so a session that launched agents but
    /// got no numbers back never reads identically to a session that
    /// launched none.
    pub subagent_launched_unstatted: u32,
    /// V0029/R3 — server-capped (`OUTCOME_WIRE_MAX_CHARS`, 240 chars)
    /// preview of the persisted `last_assistant_text` column: the session's
    /// CLOSURE, the peer of `first_user_prompt`. `None` for a prose-less husk
    /// or an un-backfilled row.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts-export", ts(optional))]
    pub outcome: Option<String>,
    /// V0029/R5 — the harness that produced this capture
    /// (`kb_core::sessions::HARNESSES`'s closed set). Never empty — the
    /// column is `NOT NULL DEFAULT 'claude'`.
    pub harness: String,
    /// V0029/P1 — the derived project key (`claude_project_slug(root)`).
    /// `None` when the derivation ladder found neither a resolved commit's
    /// repo root nor a cwd.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts-export", ts(optional))]
    pub project_key: Option<String>,
    /// V0029/S1 — the deterministic triage enum
    /// (`"trivial"|"routine"|"substantive"`). `None` means un-backfilled;
    /// every filter-layer reader MUST treat `None` as `"substantive"`.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts-export", ts(optional))]
    pub substance: Option<String>,
    /// V0029/R6/D4 — the honest ACTIVE duration in seconds (per-delta
    /// clamped sum), vs. `duration_ms`'s wall-clock span.
    pub active_secs: i64,
    /// V0029/P2 — the honest "real prompts" count (wrapper-skipping), vs.
    /// `message_count`'s raw JSONL record count.
    pub user_turns: u32,
    /// W3.B/P10 — count of `kind="commit"` rows this capture wrote to
    /// `session_commits` (push/tag excluded). Was persisted (V0029) but
    /// never threaded onto the wire; the S1 list-row `✓N` commit badge and
    /// the S7 gallery card both need it here rather than a second
    /// `/commits` fetch per row.
    pub commit_count: u32,
}

/// A2/A3 — the server-computed "display name" ladder: `title` →
/// `first_user_prompt` → short session id, all trim-checked for emptiness but
/// returned untrimmed. Shared by [`SessionOut::from_row`] and the W0.6
/// `by_commit` match resolver so every surface agrees. Never empty.
fn display_name_of(
    title: Option<&str>,
    first_user_prompt: Option<&str>,
    session_id: &str,
) -> String {
    title
        .filter(|t| !t.trim().is_empty())
        .or_else(|| first_user_prompt.filter(|p| !p.trim().is_empty()))
        .map(str::to_string)
        .unwrap_or_else(|| {
            let short: String = session_id.chars().take(8).collect();
            format!("session {short}")
        })
}

impl SessionOut {
    /// Build the wire DTO from a persisted `SessionRow`. `display_name` and
    /// `folder` are computed here (once, server-side) so the SPA and CLI
    /// never re-derive them differently. `memory_count` is filled later by
    /// the cross-kb fan-out; `title` comes from the persisted column (no
    /// lance read).
    pub fn from_row(kb: &str, row: kb_core::storage::sqlite::SessionRow) -> Self {
        let display_name = display_name_of(
            row.title.as_deref(),
            row.first_user_prompt.as_deref(),
            &row.session_id,
        );
        let folder = row.cwd.as_deref().map(|c| {
            c.trim_end_matches('/')
                .rsplit('/')
                .next()
                .unwrap_or(c)
                .to_string()
        });
        let outcome = row.last_assistant_text.as_deref().map(|t| {
            kb_core::sessions::truncate_chars_ellipsis(t, kb_core::sessions::OUTCOME_WIRE_MAX_CHARS)
        });
        SessionOut {
            id: row.session_id.clone(),
            kb: kb.to_string(),
            artifact_id: row.artifact_id,
            session_id: row.session_id,
            started_at: row.started_at,
            ended_at: row.ended_at,
            duration_ms: (row.ended_at - row.started_at).max(0) * 1000,
            message_count: row.message_count,
            first_user_prompt: row.first_user_prompt,
            memory_count: 0,
            source_relative: row.source_relative,
            title: row.title,
            display_name,
            cwd: row.cwd,
            folder,
            git_branch: row.git_branch,
            files_read_count: row.files_read_count,
            files_edited_count: row.files_edited_count,
            token_total: row.token_total,
            tool_calls: row.tool_calls,
            model: row.model,
            error_count: row.error_count,
            subagent_count: row.subagent_count,
            subagent_tokens: row.subagent_tokens,
            subagent_tool_calls: row.subagent_tool_calls,
            subagent_files_edited: row.subagent_files_edited,
            subagent_launched_unstatted: row.subagent_launched_unstatted,
            outcome,
            harness: row.harness,
            project_key: row.project_key,
            substance: row.substance,
            active_secs: row.active_secs,
            user_turns: row.user_turns,
            commit_count: row.commit_count,
        }
    }
}

#[derive(Debug, Deserialize, Default)]
pub struct ListParams {
    /// T5 — newest-first cursor: `started_at` of the last row of the
    /// previous page. Absent → page 1.
    pub cursor: Option<i64>,
    /// X1 — the compound half of the keyset cursor: `artifact_id` of the
    /// last row of the previous page. Paired with `cursor` so a page
    /// boundary inside a same-`started_at` group doesn't drop/duplicate
    /// rows. Absent (legacy) → strict `started_at < cursor`.
    pub cursor_id: Option<String>,
    /// T5 — per-page cap, clamped to MAX_LIMIT. Absent → DEFAULT_LIMIT.
    pub limit: Option<u32>,
    /// A1 — restrict to one working directory (the full `cwd` path, as
    /// returned by `/api/sessions/folders`). Applied per-kb in SQL before the
    /// cross-kb merge so keyset pagination within the folder stays coherent.
    pub folder: Option<String>,
    /// P6 — keyword filter (title / first prompt / cwd), case-insensitive.
    pub q: Option<String>,
    /// W3.A — restrict to one project: a registered `[projects.*]` id, OR
    /// (when it doesn't match one) a raw `project_key`. `folder=` keeps
    /// working unchanged (a cwd filter) — this is the P1-derived-key axis,
    /// legacy `folder=` inbound per R12/C-17.
    pub project: Option<String>,
    /// W3.A/S1 — csv over the triage enum (`trivial|routine|substantive`).
    /// Absent/empty = no filter (every session shown; `NULL` substance rows
    /// are NEVER hidden by omission).
    pub substance: Option<String>,
    /// W5/I — csv over `kb_core::sessions::HARNESSES`
    /// (`claude|codex|opencode|grok|kimi|omp`). Absent/empty = no filter.
    /// *OK1 amendment:* an EXPLICITLY provided token outside the closed set
    /// 400s (mirrors the beat route's "unknown beat harness" error) rather
    /// than silently dropping — a caller that typos (or targets a harness
    /// this build doesn't know about yet) deserves an honest error, not a
    /// query that quietly matched fewer harnesses than it asked for.
    pub harness: Option<String>,
}

/// W5/I — parse a csv `?harness=` value into the set to filter by.
/// Absent/empty input parses to an empty `Vec` (== "no filter" — callers
/// only invoke this when `?harness=` was actually present, via
/// `Option::as_deref().map(...)`, so an absent param never reaches here).
/// *OK1 amendment (2026-08):* an EXPLICITLY provided token outside the
/// closed `kb_core::sessions::HARNESSES` set now 400s (`Err` carrying a
/// ready-to-return problem+json `Response`, mirroring `beat`'s "unknown
/// beat harness" error) instead of being silently dropped — the old
/// drop-unknown behaviour meant `?harness=omp` (a real, already-captured
/// harness the closed set hadn't caught up to yet) quietly degraded to
/// "no filter" rather than erroring or matching. Blank entries are still
/// dropped (mirrors [`parse_substance_csv`]'s blank-entry handling) since
/// they carry no signal to reject.
///
/// The `Err` variant is a fully-built `Response` (mirrors
/// `routes::resolve_kb`'s documented idiom) so callers can `return` it
/// directly; the error path is the rare malformed-token case, so the
/// `result_large_err` cost is accepted over boxing it at every call site.
#[allow(clippy::result_large_err)]
fn parse_harness_csv(raw: &str) -> Result<Vec<String>, Response> {
    raw.split(',')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(|s| {
            if kb_core::sessions::HARNESSES.contains(&s) {
                Ok(s.to_string())
            } else {
                Err(error_to_problem_json(&kb_core::Error::BadRequest(format!(
                    "unknown harness filter value {s:?} (expected one of: {:?})",
                    kb_core::sessions::HARNESSES
                ))))
            }
        })
        .collect()
}

/// W3.A — parse a csv `?substance=` value into the set `sessions_list`
/// filters on. Blank entries are dropped; an entirely-blank input parses to
/// an empty `Vec` (== "no filter").
fn parse_substance_csv(raw: &str) -> Vec<String> {
    raw.split(',')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .collect()
}

#[cfg(test)]
mod harness_csv_tests {
    use super::parse_harness_csv;

    /// OK1 — a csv of every closed-set member, INCLUDING `omp`, parses
    /// clean and every token survives: the regression this whole phase
    /// exists to fix was `?harness=omp` silently degrading to "no filter"
    /// (the token was dropped, not rejected) because `omp` wasn't yet a
    /// member of `kb_core::sessions::HARNESSES`. Now that it is, this pins
    /// that `omp` actually filters rather than vanishing from the set.
    #[test]
    fn every_closed_set_member_including_omp_survives_parsing() {
        let got = parse_harness_csv("claude,codex,opencode,grok,kimi,omp").unwrap();
        assert_eq!(
            got,
            vec!["claude", "codex", "opencode", "grok", "kimi", "omp"]
        );
        // Single-token form too — the shape `?harness=omp` actually sends.
        assert_eq!(parse_harness_csv("omp").unwrap(), vec!["omp"]);
    }

    /// Absent-vs-empty stays "no filter": blank entries (including an
    /// entirely blank string) parse to an empty `Vec`, never an error.
    #[test]
    fn blank_and_empty_input_parses_to_no_filter() {
        assert_eq!(parse_harness_csv("").unwrap(), Vec::<String>::new());
        assert_eq!(parse_harness_csv(" , ,").unwrap(), Vec::<String>::new());
    }

    /// OK1 — an EXPLICITLY provided unknown token 400s (problem+json),
    /// rather than the old silent-drop-to-unfiltered behaviour. Mirrors the
    /// beat route's "unknown beat harness" error shape.
    #[test]
    fn unknown_explicit_token_400s_instead_of_silently_dropping() {
        let resp = parse_harness_csv("codex,bogus-harness").unwrap_err();
        assert_eq!(resp.status(), axum::http::StatusCode::BAD_REQUEST);
    }

    /// A known token alongside an unknown one still 400s the WHOLE
    /// request — never a partial "matched the known ones, dropped the
    /// rest" result, which would be just as silent a degrade as before.
    #[test]
    fn one_bad_token_rejects_the_whole_csv_even_with_good_tokens_present() {
        assert!(parse_harness_csv("claude,nope").is_err());
        assert!(parse_harness_csv("nope,claude").is_err());
    }
}

#[cfg_attr(feature = "ts-export", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Serialize)]
pub struct SessionsListResponse {
    pub sessions: Vec<SessionOut>,
    /// T5 — `started_at` of the last row in `sessions`. Absent when
    /// the page was empty OR shorter than the requested limit (no
    /// next page).
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts-export", ts(optional))]
    pub next_cursor: Option<i64>,
    /// X1 — `artifact_id` of the last row; the compound half of the
    /// keyset cursor. Echoed back as `cursor_id` on the next request.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts-export", ts(optional))]
    pub next_cursor_id: Option<String>,
}

#[cfg_attr(
    feature = "ts-export",
    derive(ts_rs::TS),
    ts(export, rename = "SessionDetail")
)]
#[derive(Debug, Serialize)]
pub struct SessionDetailResponse {
    #[serde(flatten)]
    pub session: SessionOut,
    /// Lance artifact ids whose `kb_session` matches this session id.
    /// The full memory rows are served separately by
    /// `/api/sessions/{sid}/memories` — this list is a cheap "yes there
    /// are N memories, here are their ids" so the SPA can render
    /// nested-row affordances without a second request.
    pub memory_ids: Vec<String>,
}

#[cfg_attr(
    feature = "ts-export",
    derive(ts_rs::TS),
    ts(export, rename = "SessionMemoryHit")
)]
#[derive(Debug, Serialize)]
pub struct MemoryHit {
    pub id: String,
    pub kb: String,
    pub title: String,
    pub path: String,
    pub source_relative: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts-export", ts(optional))]
    pub mtime_unix: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts-export", ts(optional))]
    pub kb_salience: Option<f32>,
}

#[cfg_attr(feature = "ts-export", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Serialize)]
pub struct SessionMemoriesResponse {
    pub memories: Vec<MemoryHit>,
}

/// MI-W4.2c — one memory this session's `kb-recall` hook actually injected
/// (the PULL side, mirroring `MemoryHit`'s WRITE side above). Resolved from
/// the session's own `memory_recalls` ledger row (`memory_kb`/`memory_id`/
/// `turn_id`/`recalled_at`) plus a `get_by_ids` lookup into `memory_kb` for
/// display fields — a memory can be recalled from a DIFFERENT corpus than
/// the one capturing this session (invariant #28), so `title`/
/// `source_relative` are best-effort: `None` when that lookup fails (kb
/// unreachable/renamed, memory since hard-deleted) rather than dropping the
/// ledger row — the recall EVENT happened regardless of whether the memory
/// still resolves today.
#[cfg_attr(
    feature = "ts-export",
    derive(ts_rs::TS),
    ts(export, rename = "SessionRecallHit")
)]
#[derive(Debug, Serialize)]
pub struct SessionRecallHit {
    pub kb: String,
    pub id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts-export", ts(optional))]
    pub title: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts-export", ts(optional))]
    pub source_relative: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts-export", ts(optional))]
    pub turn_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts-export", ts(optional))]
    pub recalled_at: Option<i64>,
    /// CT-C5 (V0037) — did a LATER turn in this same session explicitly
    /// reference this memory (its id or title)? See
    /// `kb_core::sessions::view::DerivedRecall::used`'s doc comment for the
    /// exact definition (explicit reference only — an agent can act on a
    /// recalled fact without ever naming it, and that case reads as
    /// `false` here too). Display only, never a scoring input.
    pub used: bool,
    /// MR1 (V0040) — the hit's RANK in the pack that injected it, 1 = top.
    /// Absent when the capture's `kb-recall/1` marker carried no `pos=`
    /// (a pre-MR1 capture, a fallback-only parse, or a mangled value) —
    /// see `kb_core::sessions::view::DerivedRecall::pos` for why nothing
    /// infers one from the hit's position in the transcript. Display only,
    /// never a scoring input.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts-export", ts(optional))]
    pub pos: Option<u32>,
}

#[cfg_attr(feature = "ts-export", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Serialize)]
pub struct SessionRecallsResponse {
    pub recalls: Vec<SessionRecallHit>,
}

#[cfg_attr(feature = "ts-export", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Serialize)]
pub struct TouchesResponse {
    pub artifact_ids: Vec<String>,
    pub confidence: kb_core::sessions::TouchesConfidence,
    /// CT-A6 — same ids as `artifact_ids`, each tagged with the join-tier
    /// (`exact`|`fuzzy`) that found it. Additive: `artifact_ids` +
    /// aggregate `confidence` are unchanged, this just gives a consumer
    /// per-row precision instead of only a whole-scan rollup.
    pub artifacts: Vec<kb_core::sessions::TouchedArtifact>,
}

/// `GET /api/sessions` — every memory-session row across every kb,
/// newest started_at first. One enrichment row per kb is joined with
/// its lance projection (for `title`) and the cross-kb
/// `count_docs_with_kb_session` (for `memory_count`).
pub async fn list(
    State(state): State<Arc<KbHandles>>,
    Query(params): Query<ListParams>,
) -> Response {
    let limit = params.limit.unwrap_or(DEFAULT_LIMIT).clamp(1, MAX_LIMIT);
    // T5 — over-fetch per kb so the cross-kb merge can still serve a
    // full page even when one corpus dominates. Each per-kb fetch
    // honours the same `before` cursor so older pages stay coherent.
    let per_kb = limit.saturating_mul(2);
    // W3.A — resolve `?project=` once (registry lookup or raw-key literal)
    // and parse `?substance=` once; both are threaded per-kb into SQL WHERE
    // (`sessions_list`) so the keyset cursor stays coherent (S-S1(b)).
    let project_filter = params
        .project
        .as_deref()
        .map(kb_core::sessions::resolve_project_filter)
        .unwrap_or_default();
    let substance: Vec<String> = params
        .substance
        .as_deref()
        .map(parse_substance_csv)
        .unwrap_or_default();
    let harness: Vec<String> = match params.harness.as_deref().map(parse_harness_csv) {
        Some(Ok(h)) => h,
        Some(Err(resp)) => return resp,
        None => Vec::new(),
    };
    // FF-D — fan out each kb's sessions_list concurrently (bounded,
    // submission-ordered), then flatten in BTreeMap order. Pure reads; the
    // deferred title/memory_count enrichment + final sort below are unchanged.
    // `title`/`memory_count` are display-only (NOT the cross-kb sort key
    // started_at/artifact_id) so they are deferred until after sort+truncate —
    // only the surviving page rows need them.
    let params = &params;
    let project_filter = &project_filter;
    let substance = &substance;
    let harness = &harness;
    let mut futs: Vec<super::CorpusFut<'_, Vec<SessionOut>>> = Vec::new();
    for (kb_name, ctx) in state.kbs.iter() {
        futs.push(Box::pin(async move {
            let rows = match ctx
                .storage
                .sessions_list(
                    per_kb,
                    params.cursor,
                    params.cursor_id.clone(),
                    params.folder.clone(),
                    params.q.clone(),
                    project_filter.clone(),
                    substance.clone(),
                    harness.clone(),
                )
                .await
            {
                Ok(r) => r,
                Err(e) => {
                    tracing::warn!(kb = %kb_name, error = %e, "sessions_list failed");
                    return Vec::new();
                }
            };
            rows.into_iter()
                .map(|row| SessionOut::from_row(kb_name.as_str(), row))
                .collect()
        }));
    }
    // PF-R1 — the operator-configurable `[server] fanout_cap` (default 8,
    // byte-identical to the old hardcoded `super::FANOUT_CAP`).
    let mut out: Vec<SessionOut> = super::buffered_join(futs, state.fanout_cap)
        .await
        .into_iter()
        .flatten()
        .collect();
    // Final cross-kb sort: newest-first by started_at, then by
    // artifact_id ASC for a deterministic tiebreak (matches the
    // sqlite ORDER BY inside each kb's `sessions_list`).
    out.sort_by(|a, b| {
        b.started_at
            .cmp(&a.started_at)
            .then_with(|| a.artifact_id.cmp(&b.artifact_id))
    });
    // T5 — truncate to the requested limit and compute the cursor.
    // The cursor is the last surfaced row's `started_at`, which the
    // next call passes back as `?cursor=`. When the over-fetch was
    // exhausted before hitting the limit there are no more pages →
    // `next_cursor: None`.
    let had_more = out.len() > limit as usize;
    out.truncate(limit as usize);
    // Second pass — the page is now fixed, so fill `memory_count` for the
    // surviving rows ONLY, via ONE batched grouped count per corpus
    // (`count_docs_by_kb_session`: one projection scan each) fanned out
    // concurrently (submission-ordered, #28) — NOT one `count_rows` filter
    // scan per row × corpus, which made the count pass the page's dominant
    // cost. A corpus that errors folds to an empty map (its docs contribute
    // 0, same as the old per-row fold); an id absent from every map renders
    // as 0. Counts stay capture-independent (#11 — `kb_session` is a doc
    // column, not a capture row). `title` no longer needs a per-row lance
    // read: it's the persisted V0017 column, projected by
    // `SessionOut::from_row` above (S4).
    let page_ids: Vec<String> = out.iter().map(|s| s.session_id.clone()).collect();
    let page_ids = &page_ids;
    let mut count_futs: Vec<super::CorpusFut<'_, HashMap<String, u64>>> = Vec::new();
    for (kb_name, ctx) in state.kbs.iter() {
        count_futs.push(Box::pin(async move {
            match ctx.storage.count_docs_by_kb_session(page_ids.clone()).await {
                Ok(m) => m,
                Err(e) => {
                    tracing::warn!(kb = %kb_name, error = %e, "count_docs_by_kb_session failed");
                    HashMap::new()
                }
            }
        }));
    }
    let mut counts: HashMap<String, u64> = HashMap::new();
    // PF-R1 — the operator-configurable `[server] fanout_cap` (default 8,
    // byte-identical to the old hardcoded `super::FANOUT_CAP`).
    for per_kb in super::buffered_join(count_futs, state.fanout_cap).await {
        for (sid, n) in per_kb {
            *counts.entry(sid).or_insert(0) += n;
        }
    }
    for s in out.iter_mut() {
        s.memory_count = counts.get(&s.session_id).copied().unwrap_or(0);
    }
    let (next_cursor, next_cursor_id) = if had_more {
        (
            out.last().map(|s| s.started_at),
            out.last().map(|s| s.artifact_id.clone()),
        )
    } else {
        (None, None)
    };
    Json(SessionsListResponse {
        sessions: out,
        next_cursor,
        next_cursor_id,
    })
    .into_response()
}

/// One entry in the A1 folder facet.
#[cfg_attr(feature = "ts-export", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Serialize)]
pub struct FolderOut {
    /// Full working-directory path — the value passed back as `?folder=`.
    pub folder: String,
    /// Display label (basename of the path).
    pub label: String,
    /// Sessions in this folder, summed across all corpora.
    pub count: u64,
    /// Most-recent session start in this folder (unix secs).
    pub latest: i64,
    /// P6 — earliest session start (the start of the project's span).
    pub earliest: i64,
    /// P6 — total files edited across the folder's sessions.
    pub edited_total: u64,
    /// P6 — total tokens across the folder's sessions.
    pub token_total: u64,
}

#[cfg_attr(feature = "ts-export", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Serialize)]
pub struct SessionFoldersResponse {
    pub folders: Vec<FolderOut>,
}

/// `GET /api/sessions/folders` — the A1 folder facet: every distinct working
/// directory across all corpora with its session count + latest activity,
/// newest-active first. Counts for the same folder seen in multiple corpora
/// are summed.
pub async fn folders(State(state): State<Arc<KbHandles>>) -> impl IntoResponse {
    let mut futs: Vec<super::CorpusFut<'_, Vec<kb_core::storage::sqlite::FolderStats>>> =
        Vec::new();
    for (kb_name, ctx) in state.kbs.iter() {
        futs.push(Box::pin(async move {
            ctx.storage.sessions_folders().await.unwrap_or_else(|e| {
                tracing::warn!(kb = %kb_name, error = %e, "sessions_folders failed");
                Vec::new()
            })
        }));
    }
    // Merge across corpora per distinct cwd: sum counts/edited/tokens, max
    // latest, min earliest.
    struct Agg {
        count: u64,
        latest: i64,
        earliest: i64,
        edited: u64,
        tokens: u64,
    }
    let mut by_folder: std::collections::BTreeMap<String, Agg> = std::collections::BTreeMap::new();
    // PF-R1 — the operator-configurable `[server] fanout_cap` (default 8,
    // byte-identical to the old hardcoded `super::FANOUT_CAP`).
    for s in super::buffered_join(futs, state.fanout_cap)
        .await
        .into_iter()
        .flatten()
    {
        let e = by_folder.entry(s.cwd).or_insert(Agg {
            count: 0,
            latest: 0,
            earliest: i64::MAX,
            edited: 0,
            tokens: 0,
        });
        e.count += s.count as u64;
        e.latest = e.latest.max(s.latest);
        e.earliest = e.earliest.min(s.earliest);
        e.edited += s.edited_total as u64;
        e.tokens += s.token_total as u64;
    }
    let mut folders: Vec<FolderOut> = by_folder
        .into_iter()
        .map(|(folder, a)| {
            let label = folder
                .trim_end_matches('/')
                .rsplit('/')
                .next()
                .unwrap_or(&folder)
                .to_string();
            FolderOut {
                folder,
                label,
                count: a.count,
                latest: a.latest,
                earliest: if a.earliest == i64::MAX {
                    0
                } else {
                    a.earliest
                },
                edited_total: a.edited,
                token_total: a.tokens,
            }
        })
        .collect();
    // Newest-active first; basename tiebreak for determinism.
    folders.sort_by(|a, b| b.latest.cmp(&a.latest).then_with(|| a.label.cmp(&b.label)));
    Json(SessionFoldersResponse { folders })
}

// ---- W3.A/P4 — the projects facet -----------------------------------------

/// One project card (designs/projects.md P4/P6): a `[projects.*]` registry
/// entry OR an auto-project (`source: "derived"`), with its cross-corpus
/// rollup.
#[cfg_attr(feature = "ts-export", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Serialize)]
pub struct ProjectOut {
    /// The value `?project=` accepts back: the registry id, or (for an
    /// auto-project) the raw `COALESCE(project_key, cwd)` key.
    pub project: String,
    pub label: String,
    /// `"registry" | "derived"`.
    pub source: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts-export", ts(optional))]
    pub kb: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts-export", ts(optional))]
    pub code_url: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts-export", ts(optional))]
    pub roots: Option<Vec<String>>,
    pub count: u64,
    pub latest: i64,
    pub earliest: i64,
    pub edited_total: u64,
    pub token_total: u64,
    pub commit_total: u64,
    pub error_sessions: u64,
    pub active_secs_total: i64,
    /// Harness label → session count, e.g. `{"claude": 40, "codex": 3}`.
    pub harness_mix: std::collections::BTreeMap<String, u64>,
}

#[cfg_attr(feature = "ts-export", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Serialize)]
pub struct SessionsProjectsResponse {
    pub projects: Vec<ProjectOut>,
}

#[derive(Default)]
struct ProjectAcc {
    label: String,
    source: &'static str,
    kb: Option<String>,
    code_url: Option<String>,
    roots: Option<Vec<String>>,
    count: i64,
    latest: i64,
    earliest: i64,
    edited: i64,
    tokens: i64,
    commits: i64,
    error_sessions: i64,
    active_secs: i64,
    harness_mix: std::collections::BTreeMap<String, i64>,
}

/// `GET /api/sessions/projects` (P4/P6) — one card per project: a declared
/// `[projects.*]` registry entry (merging every raw `COALESCE(project_key,
/// cwd)` key whose `repo_root`/`cwd` falls under one of its roots) or, for
/// anything unmatched, an auto-project (`id = key`, `label = basename`).
/// Federated (#28), newest-capture scoped at the sqlite layer (#11).
pub async fn projects(State(state): State<Arc<KbHandles>>) -> impl IntoResponse {
    let mut stat_futs: Vec<super::CorpusFut<'_, Vec<kb_core::storage::sqlite::ProjectStatsRow>>> =
        Vec::new();
    let mut harness_futs: Vec<
        super::CorpusFut<'_, Vec<kb_core::storage::sqlite::ProjectHarnessRow>>,
    > = Vec::new();
    for (kb_name, ctx) in state.kbs.iter() {
        stat_futs.push(Box::pin(async move {
            ctx.storage
                .sessions_projects_stats()
                .await
                .unwrap_or_else(|e| {
                    tracing::warn!(kb = %kb_name, error = %e, "sessions_projects_stats failed");
                    Vec::new()
                })
        }));
        harness_futs.push(Box::pin(async move {
            ctx.storage
                .sessions_projects_harness_mix()
                .await
                .unwrap_or_else(|e| {
                    tracing::warn!(kb = %kb_name, error = %e, "sessions_projects_harness_mix failed");
                    Vec::new()
                })
        }));
    }
    // PF-R1 — the operator-configurable `[server] fanout_cap` (default 8,
    // byte-identical to the old hardcoded `super::FANOUT_CAP`).
    let stats = super::buffered_join(stat_futs, state.fanout_cap)
        .await
        .into_iter()
        .flatten()
        .collect::<Vec<_>>();
    let harness_rows = super::buffered_join(harness_futs, state.fanout_cap)
        .await
        .into_iter()
        .flatten()
        .collect::<Vec<_>>();

    // Pass 1: merge cross-kb by the raw key, keeping a resolved-id lookup so
    // pass 2 can fold the (also raw-keyed) harness rows under the SAME id.
    let mut key_to_id: std::collections::HashMap<String, String> = std::collections::HashMap::new();
    let mut by_id: std::collections::BTreeMap<String, ProjectAcc> =
        std::collections::BTreeMap::new();
    for row in stats {
        let resolved = kb_core::sessions::resolve_registry_project(
            row.repo_root.as_deref(),
            row.cwd_sample.as_deref(),
        );
        let (id, label, source, kb, code_url, roots) = match resolved {
            Some(rp) => {
                let roots = kb_core::sessions::registry_entry(&rp.id).map(|d| d.roots);
                (rp.id, rp.label, rp.source, rp.kb, rp.code_url, roots)
            }
            None => {
                let basis = row
                    .repo_root
                    .as_deref()
                    .or(row.cwd_sample.as_deref())
                    .unwrap_or(&row.key);
                let label = basis
                    .trim_end_matches('/')
                    .rsplit('/')
                    .next()
                    .unwrap_or(basis)
                    .to_string();
                (row.key.clone(), label, "derived", None, None, None)
            }
        };
        key_to_id.insert(row.key.clone(), id.clone());
        let acc = by_id.entry(id).or_insert_with(|| ProjectAcc {
            label: label.clone(),
            source,
            kb: kb.clone(),
            code_url: code_url.clone(),
            roots: roots.clone(),
            earliest: i64::MAX,
            ..Default::default()
        });
        acc.count += row.count;
        acc.latest = acc.latest.max(row.latest);
        acc.earliest = acc.earliest.min(row.earliest);
        acc.edited += row.edited_total;
        acc.tokens += row.token_total;
        acc.commits += row.commit_total;
        acc.error_sessions += row.error_sessions;
        acc.active_secs += row.active_secs_total;
    }
    // Pass 2: fold the harness breakdown under the same resolved id.
    for row in harness_rows {
        let Some(id) = key_to_id.get(&row.key) else {
            continue;
        };
        if let Some(acc) = by_id.get_mut(id) {
            *acc.harness_mix.entry(row.harness).or_insert(0) += row.count;
        }
    }

    let mut projects: Vec<ProjectOut> = by_id
        .into_iter()
        .map(|(id, acc)| ProjectOut {
            project: id,
            label: acc.label,
            source: acc.source,
            kb: acc.kb,
            code_url: acc.code_url,
            roots: acc.roots,
            count: acc.count.max(0) as u64,
            latest: acc.latest,
            earliest: if acc.earliest == i64::MAX {
                0
            } else {
                acc.earliest
            },
            edited_total: acc.edited.max(0) as u64,
            token_total: acc.tokens.max(0) as u64,
            commit_total: acc.commits.max(0) as u64,
            error_sessions: acc.error_sessions.max(0) as u64,
            active_secs_total: acc.active_secs,
            harness_mix: acc
                .harness_mix
                .into_iter()
                .map(|(h, n)| (h, n.max(0) as u64))
                .collect(),
        })
        .collect();
    // Newest-active first; label tiebreak for determinism.
    projects.sort_by(|a, b| b.latest.cmp(&a.latest).then_with(|| a.label.cmp(&b.label)));
    Json(SessionsProjectsResponse { projects })
}

// ---- R9 — research rollups + the activity funnel --------------------------

/// Basename of a working-directory path (the folder facet label).
fn folder_basename(cwd: &str) -> String {
    cwd.trim_end_matches('/')
        .rsplit('/')
        .next()
        .unwrap_or(cwd)
        .to_string()
}

/// R9 — one `(kind, query)` research aggregate within a folder.
#[cfg_attr(feature = "ts-export", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Serialize)]
pub struct ResearchRollupOut {
    pub kind: String,
    pub query: String,
    pub count: u64,
    pub sessions: u64,
}

/// R9 — a project folder + its top research queries.
#[cfg_attr(feature = "ts-export", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Serialize)]
pub struct ResearchRollupFolderOut {
    pub folder: String,
    pub label: String,
    pub latest: i64,
    pub queries: Vec<ResearchRollupOut>,
}

#[cfg_attr(feature = "ts-export", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Serialize)]
pub struct ResearchRollupResponse {
    pub folders: Vec<ResearchRollupFolderOut>,
}

#[derive(Debug, Deserialize, Default)]
pub struct RollupParams {
    pub folder: Option<String>,
    /// W3.A — restrict to one project (registered id or raw `project_key`).
    /// Applied Rust-side after the cross-kb merge (not cursor-paginated —
    /// unlike `sessions_list`, a post-fetch filter here can't desync
    /// anything).
    pub project: Option<String>,
    /// L1/F1 — csv over the triage enum (trivial|routine|substantive).
    pub substance: Option<String>,
    pub limit: Option<u32>,
}

const ROLLUP_DEFAULT_TOP_N: u32 = 5;
const ROLLUP_MAX_TOP_N: u32 = 50;

/// `GET /api/sessions/research-rollup?folder=&project=&limit=` (R9) — the top
/// research queries per project folder ("what has this project been
/// researching"), merged cross-corpus. Deterministic + LLM-free (#10): GROUP
/// BY over `session_research` joined to `sessions.cwd`. `?folder=` accepts
/// the full cwd or its basename; `?project=` (W3.A) is the P1-derived-key
/// axis (registry id or raw `project_key`) — the two compose (AND) when both
/// are given. Federated (#28).
pub async fn research_rollup(
    State(state): State<Arc<KbHandles>>,
    Query(p): Query<RollupParams>,
) -> impl IntoResponse {
    let top_n = p
        .limit
        .unwrap_or(ROLLUP_DEFAULT_TOP_N)
        .clamp(1, ROLLUP_MAX_TOP_N) as usize;
    let want_folder = p.folder.as_deref().map(str::trim).filter(|s| !s.is_empty());
    let project_filter = p
        .project
        .as_deref()
        .map(kb_core::sessions::resolve_project_filter)
        .unwrap_or_default();
    let substance: Vec<String> = p
        .substance
        .as_deref()
        .map(|s| s.split(',').map(|x| x.to_string()).collect())
        .unwrap_or_default();

    let mut futs: Vec<super::CorpusFut<'_, Vec<kb_core::storage::sqlite::ResearchRollupRow>>> =
        Vec::new();
    for (kb_name, ctx) in state.kbs.iter() {
        let substance_vec = substance.clone();
        futs.push(Box::pin(async move {
            ctx.storage
                .sessions_research_rollup(substance_vec)
                .await
                .unwrap_or_else(|e| {
                    tracing::warn!(kb = %kb_name, error = %e, "sessions_research_rollup failed");
                    Vec::new()
                })
        }));
    }
    // Merge cross-corpus: (cwd, kind, query) → (count, sessions, latest).
    // `project_key` is tracked per-cwd (first-wins; it's an MIN-picked
    // representative already at the SQL layer) for the `?project=` filter.
    type Key = (String, String, String);
    let mut merged: std::collections::BTreeMap<Key, (u64, u64, i64)> =
        std::collections::BTreeMap::new();
    let mut cwd_project_key: std::collections::BTreeMap<String, Option<String>> =
        std::collections::BTreeMap::new();
    // PF-R1 — the operator-configurable `[server] fanout_cap` (default 8,
    // byte-identical to the old hardcoded `super::FANOUT_CAP`).
    for r in super::buffered_join(futs, state.fanout_cap)
        .await
        .into_iter()
        .flatten()
    {
        cwd_project_key
            .entry(r.cwd.clone())
            .or_insert_with(|| r.project_key.clone());
        let e = merged.entry((r.cwd, r.kind, r.query)).or_insert((0, 0, 0));
        e.0 += r.count as u64;
        e.1 += r.sessions as u64;
        e.2 = e.2.max(r.latest);
    }
    // Group by folder; optional filters (full cwd/basename AND project).
    let mut by_folder: std::collections::BTreeMap<String, (i64, Vec<ResearchRollupOut>)> =
        std::collections::BTreeMap::new();
    for ((cwd, kind, query), (count, sessions, latest)) in merged {
        if let Some(f) = want_folder {
            if cwd != f && folder_basename(&cwd) != f {
                continue;
            }
        }
        if !project_filter.is_empty() {
            let pk = cwd_project_key.get(&cwd).cloned().flatten();
            if !project_filter.matches(pk.as_deref(), Some(&cwd)) {
                continue;
            }
        }
        let entry = by_folder.entry(cwd).or_insert((0, Vec::new()));
        entry.0 = entry.0.max(latest);
        entry.1.push(ResearchRollupOut {
            kind,
            query,
            count,
            sessions,
        });
    }
    let mut folders: Vec<ResearchRollupFolderOut> = by_folder
        .into_iter()
        .map(|(folder, (latest, mut queries))| {
            // Top-N within the folder: count DESC, then query ASC.
            queries.sort_by(|a, b| b.count.cmp(&a.count).then_with(|| a.query.cmp(&b.query)));
            queries.truncate(top_n);
            let label = folder_basename(&folder);
            ResearchRollupFolderOut {
                folder,
                label,
                latest,
                queries,
            }
        })
        .collect();
    folders.sort_by(|a, b| b.latest.cmp(&a.latest).then_with(|| a.label.cmp(&b.label)));
    Json(ResearchRollupResponse { folders })
}

/// R9 — one funnel stage's event + distinct-session counts.
#[cfg_attr(feature = "ts-export", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Serialize)]
pub struct FunnelStageOut {
    pub stage: String,
    pub events: u64,
    pub sessions: u64,
}

#[cfg_attr(feature = "ts-export", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Serialize)]
pub struct FunnelResponse {
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts-export", ts(optional))]
    pub folder: Option<String>,
    /// Ordered: searched → opened → edited → committed → commented.
    pub stages: Vec<FunnelStageOut>,
}

#[derive(Debug, Deserialize, Default)]
pub struct FunnelParams {
    pub folder: Option<String>,
    /// W3.A — restrict to one project (registry id or raw `project_key`),
    /// threaded into the SQL WHERE the same as `folder`. NOTE: the
    /// `commented` stage (below) is folder-scoped only — it loads review
    /// files per touched artifact via `session_files_in_folder`, which
    /// wasn't extended with a project axis this wave (a documented scope
    /// trim; `project=` alone still narrows the first four stages).
    pub project: Option<String>,
    /// L1/F1 — csv over the triage enum (trivial|routine|substantive).
    pub substance: Option<String>,
}

/// `GET /api/sessions/funnel?folder=&project=` (R9) — the activity funnel
/// searched → opened → edited → committed → commented, overall or per project.
/// Each stage carries total events + distinct sessions reaching it. The first
/// four stages are cheap cross-table COUNTs over the session_* tables; the
/// `commented` stage loads review files (#6). Deterministic + LLM-free (#10);
/// "detected, not ground truth" (the `opened` stage is a coarse signal).
pub async fn funnel(
    State(state): State<Arc<KbHandles>>,
    Query(p): Query<FunnelParams>,
) -> impl IntoResponse {
    let folder = p.folder.as_deref().map(str::trim).filter(|s| !s.is_empty());
    let folder_owned = folder.map(|s| s.to_string());
    let project_filter = p
        .project
        .as_deref()
        .map(kb_core::sessions::resolve_project_filter)
        .unwrap_or_default();
    let substance: Vec<String> = p
        .substance
        .as_deref()
        .map(|s| s.split(',').map(|x| x.to_string()).collect())
        .unwrap_or_default();

    // a) the sqlite stages, summed across corpora.
    let folder_ref = &folder_owned;
    let project_ref = &project_filter;
    let substance_ref = &substance;
    let mut futs: Vec<super::CorpusFut<'_, kb_core::storage::sqlite::FunnelCounts>> = Vec::new();
    for (kb_name, ctx) in state.kbs.iter() {
        let substance_vec = substance_ref.clone();
        futs.push(Box::pin(async move {
            ctx.storage
                .sessions_funnel_counts(folder_ref.clone(), project_ref.clone(), substance_vec)
                .await
                .unwrap_or_else(|e| {
                    tracing::warn!(kb = %kb_name, error = %e, "sessions_funnel_counts failed");
                    Default::default()
                })
        }));
    }
    // PF-R1 — the operator-configurable `[server] fanout_cap` (default 8,
    // byte-identical to the old hardcoded `super::FANOUT_CAP`).
    let agg = super::buffered_join(futs, state.fanout_cap)
        .await
        .into_iter()
        .fold(
            kb_core::storage::sqlite::FunnelCounts::default(),
            |mut a, c| {
                a.searched_events += c.searched_events;
                a.searched_sessions += c.searched_sessions;
                a.opened_events += c.opened_events;
                a.opened_sessions += c.opened_sessions;
                a.edited_events += c.edited_events;
                a.edited_sessions += c.edited_sessions;
                a.committed_events += c.committed_events;
                a.committed_sessions += c.committed_sessions;
                a
            },
        );

    // b) the commented stage from the review files.
    let (commented_events, commented_sessions) = funnel_commented(&state, &folder_owned).await;

    let stages = vec![
        FunnelStageOut {
            stage: "searched".into(),
            events: agg.searched_events as u64,
            sessions: agg.searched_sessions as u64,
        },
        FunnelStageOut {
            stage: "opened".into(),
            events: agg.opened_events as u64,
            sessions: agg.opened_sessions as u64,
        },
        FunnelStageOut {
            stage: "edited".into(),
            events: agg.edited_events as u64,
            sessions: agg.edited_sessions as u64,
        },
        FunnelStageOut {
            stage: "committed".into(),
            events: agg.committed_events as u64,
            sessions: agg.committed_sessions as u64,
        },
        FunnelStageOut {
            stage: "commented".into(),
            events: commented_events,
            sessions: commented_sessions,
        },
    ];
    Json(FunnelResponse {
        folder: folder_owned,
        stages,
    })
}

/// R9 — the funnel's `commented` stage: open comments on the in-corpus
/// artifacts a folder's sessions touched, and how many of those sessions
/// touched a commented artifact. Reuses the R5 review-load path (#6),
/// federated (#28).
async fn funnel_commented(state: &Arc<KbHandles>, folder: &Option<String>) -> (u64, u64) {
    let folder_ref = folder;
    let mut futs: Vec<super::CorpusFut<'_, Vec<(String, String, String)>>> = Vec::new();
    for (kb_name, ctx) in state.kbs.iter() {
        futs.push(Box::pin(async move {
            ctx.storage
                .session_files_in_folder(folder_ref.clone())
                .await
                .unwrap_or_else(|e| {
                    tracing::warn!(kb = %kb_name, error = %e, "session_files_in_folder failed");
                    Vec::new()
                })
        }));
    }
    // (kb, artifact_id) → the session ids that touched it.
    let mut by_artifact: std::collections::BTreeMap<
        (String, String),
        std::collections::BTreeSet<String>,
    > = std::collections::BTreeMap::new();
    // PF-R1 — the operator-configurable `[server] fanout_cap` (default 8,
    // byte-identical to the old hardcoded `super::FANOUT_CAP`).
    for (sid, kb, aid) in super::buffered_join(futs, state.fanout_cap)
        .await
        .into_iter()
        .flatten()
    {
        by_artifact.entry((kb, aid)).or_default().insert(sid);
    }
    let by_artifact_ref = &by_artifact;
    let mut rfuts: Vec<super::CorpusFut<'_, (u64, std::collections::BTreeSet<String>)>> =
        Vec::new();
    for (kb_name, _ctx) in state.kbs.iter() {
        let review_dir = state.paths.kb_review_dir(kb_name);
        rfuts.push(Box::pin(async move {
            let mut events = 0u64;
            let mut sessions = std::collections::BTreeSet::new();
            for ((kb, aid), sids) in by_artifact_ref.iter() {
                if kb != kb_name.as_str() {
                    continue;
                }
                let Ok(Some(file)) = kb_core::review::load(&review_dir.join(format!("{aid}.json")))
                else {
                    continue;
                };
                let open = file
                    .comments
                    .iter()
                    .filter(|c| c.status == kb_core::review::CommentStatus::Open)
                    .count() as u64;
                if open > 0 {
                    events += open;
                    sessions.extend(sids.iter().cloned());
                }
            }
            (events, sessions)
        }));
    }
    let mut total_events = 0u64;
    let mut all_sessions: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
    // PF-R1 — the operator-configurable `[server] fanout_cap` (default 8,
    // byte-identical to the old hardcoded `super::FANOUT_CAP`).
    for (ev, sids) in super::buffered_join(rfuts, state.fanout_cap).await {
        total_events += ev;
        all_sessions.extend(sids);
    }
    (total_events, all_sessions.len() as u64)
}

// ─── W6 — the project ledger (moonshots M4) ────────────────────────────

/// Default/maximum window for `GET /api/sessions/ledger` — the ratified
/// shape (memo R12: "rides `project=` from day one"). A week by default; a
/// month is the ceiling (an e-ink-daycard-style glance, not a dashboard —
/// the non-goal fence M4 names explicitly).
const LEDGER_DEFAULT_DAYS: u32 = 7;
const LEDGER_MAX_DAYS: u32 = 31;
/// Upper bound on sessions scanned per kb — same reasoning + same number as
/// `THREAD_SCAN_CAP` (a personal corpus's 31-day window is well under this;
/// graceful truncation over an unbounded scan for the pathological case).
const LEDGER_SCAN_CAP: u32 = 2000;
/// Research topics kept per day — "top-3" (moonshots M4's exact wire shape).
const LEDGER_TOPICS_PER_DAY: usize = 3;

#[derive(Debug, Deserialize, Default)]
pub struct LedgerParams {
    /// W3.A axis — a registered `[projects.*]` id, or a raw `project_key`.
    /// Absent = no project filter (every session in the window, across every
    /// project) — same "compose, don't require" posture as `funnel`/
    /// `research_rollup`.
    pub project: Option<String>,
    /// Trailing window size in UTC calendar days, INCLUSIVE of today.
    /// Clamped to `[1, LEDGER_MAX_DAYS]`; absent → `LEDGER_DEFAULT_DAYS`.
    pub days: Option<u32>,
}

#[cfg_attr(feature = "ts-export", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Serialize)]
pub struct LedgerSessionOut {
    /// The session id (`SessionOut::id`) — the ledger's own stable handle,
    /// distinct from the underlying `artifact_id` (multi-capture, #11).
    pub sid: String,
    pub kb: String,
    pub display_name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts-export", ts(optional))]
    pub outcome: Option<String>,
    pub active_secs: i64,
}

#[cfg_attr(feature = "ts-export", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Serialize)]
pub struct LedgerCommitOut {
    /// `"commit" | "push" | "tag"`.
    pub kind: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts-export", ts(optional))]
    pub sha: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts-export", ts(optional))]
    pub subject: Option<String>,
    pub resolved: bool,
}

#[cfg_attr(feature = "ts-export", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Serialize)]
pub struct LedgerDayOut {
    /// UTC calendar date, `YYYY-MM-DD`.
    pub date: String,
    /// Newest-first, same convention as every other sessions list.
    pub sessions: Vec<LedgerSessionOut>,
    pub commits: Vec<LedgerCommitOut>,
    pub decisions_count: u32,
    /// Up to `LEDGER_TOPICS_PER_DAY` research queries, ranked by frequency
    /// within the day (ties broken alphabetically — deterministic, #10).
    pub research_topics: Vec<String>,
}

#[cfg_attr(feature = "ts-export", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Serialize, Default)]
pub struct LedgerTotalsOut {
    pub sessions: u32,
    pub commits: u32,
    pub decisions: u32,
    pub active_secs: i64,
}

#[cfg_attr(feature = "ts-export", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Serialize)]
pub struct LedgerResponse {
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts-export", ts(optional))]
    pub project: Option<String>,
    /// The resolved (clamped) window size — echoes what was actually
    /// applied, not the raw (possibly out-of-range) query param.
    pub days: u32,
    /// EXACTLY `days` entries, oldest first — every calendar day in the
    /// window is present even when empty (a complete, deterministic grid;
    /// the SPA/CLI never has to backfill gaps).
    pub days_out: Vec<LedgerDayOut>,
    pub totals: LedgerTotalsOut,
}

/// One session's ledger contribution, gathered inside its owning kb's
/// fan-out future (commits/decisions/research are per-session sqlite reads
/// on the SAME kb — no second fan-out layer needed, #28). `started_at` is
/// carried only for the newest-first sort within a day; it never reaches
/// the wire (see [`LedgerSessionOut`]).
struct LedgerSessionRow {
    date: String,
    started_at: i64,
    session: LedgerSessionOut,
    commits: Vec<LedgerCommitOut>,
    decisions: u32,
    research: Vec<String>,
}

/// `GET /api/sessions/ledger?project=&days=` (moonshots M4) — a VIEW over
/// EXISTING primitives, the recorded daycard precedent
/// (`routes/daycard.rs`'s module doc: "a VIEW over EXISTING primitives...
/// deterministic given (corpus state, day)"): no new tables, no new
/// aggregation state. Groups the newest-capture-scoped (#11) sessions for
/// `project=` within the trailing `days=` UTC window by calendar day, each
/// day carrying its sessions (outcome + active time), the commits/decisions/
/// research those sessions produced. Federated (#28); a bare project=/days=
/// composition query, same posture as `funnel`/`research_rollup`.
pub async fn ledger(
    State(state): State<Arc<KbHandles>>,
    Query(p): Query<LedgerParams>,
) -> impl IntoResponse {
    let days = p
        .days
        .unwrap_or(LEDGER_DEFAULT_DAYS)
        .clamp(1, LEDGER_MAX_DAYS);
    let project_filter = p
        .project
        .as_deref()
        .map(kb_core::sessions::resolve_project_filter)
        .unwrap_or_default();

    let today = chrono::Utc::now().date_naive();
    let start_date = today - chrono::Duration::days((days - 1) as i64);
    let window_start = start_date
        .and_hms_opt(0, 0, 0)
        .expect("valid midnight")
        .and_utc()
        .timestamp();

    let project_filter_ref = &project_filter;
    let mut futs: Vec<super::CorpusFut<'_, Vec<LedgerSessionRow>>> = Vec::new();
    for (kb_name, ctx) in state.kbs.iter() {
        futs.push(Box::pin(async move {
            let rows = match ctx
                .storage
                .sessions_list(
                    LEDGER_SCAN_CAP,
                    None,
                    None,
                    None,
                    None,
                    project_filter_ref.clone(),
                    Vec::new(),
                    Vec::new(),
                )
                .await
            {
                Ok(r) => r,
                Err(e) => {
                    tracing::warn!(kb = %kb_name, error = %e, "sessions_list failed in ledger");
                    return Vec::new();
                }
            };
            // Collect in-window sessions first, then ONE batch call each for
            // commits/decisions/research (PSRV-1) — was 3×N serial awaits.
            // `sessions_list` is `ORDER BY started_at DESC` — the first row
            // older than the window means every remaining row is too
            // (`sessions_list_sorts_newest_first_with_id_tiebreak` pins it).
            let mut window_rows = Vec::new();
            for row in rows {
                if row.started_at < window_start {
                    break;
                }
                window_rows.push(row);
            }
            let session_ids: Vec<String> = window_rows
                .iter()
                .map(|r| r.session_id.clone())
                .collect();
            let mut commits_map = ctx
                .storage
                .session_commits_for_sessions(session_ids.clone())
                .await
                .unwrap_or_else(|e| {
                    tracing::warn!(kb = %kb_name, error = %e, "session_commits_for_sessions failed in ledger");
                    Default::default()
                });
            let mut decisions_map = ctx
                .storage
                .session_decisions_for_sessions(session_ids.clone())
                .await
                .unwrap_or_else(|e| {
                    tracing::warn!(kb = %kb_name, error = %e, "session_decisions_for_sessions failed in ledger");
                    Default::default()
                });
            let mut research_map = ctx
                .storage
                .session_research_for_sessions(session_ids)
                .await
                .unwrap_or_else(|e| {
                    tracing::warn!(kb = %kb_name, error = %e, "session_research_for_sessions failed in ledger");
                    Default::default()
                });
            let mut out = Vec::with_capacity(window_rows.len());
            for row in window_rows {
                let date = chrono::DateTime::from_timestamp(row.started_at, 0)
                    .map(|dt| dt.date_naive().to_string())
                    .unwrap_or_default();
                let session_id = row.session_id.clone();
                let so = SessionOut::from_row(kb_name.as_str(), row);
                let commits = commits_map
                    .remove(&session_id)
                    .unwrap_or_default()
                    .into_iter()
                    .map(|c| LedgerCommitOut {
                        kind: c.kind,
                        sha: c.sha,
                        subject: c.subject,
                        resolved: c.resolved,
                    })
                    .collect();
                let decisions = decisions_map
                    .remove(&session_id)
                    .map(|d| d.len() as u32)
                    .unwrap_or(0);
                let research = research_map
                    .remove(&session_id)
                    .unwrap_or_default()
                    .into_iter()
                    .map(|r| r.query)
                    .collect();
                out.push(LedgerSessionRow {
                    date,
                    started_at: so.started_at,
                    session: LedgerSessionOut {
                        sid: so.id,
                        kb: so.kb,
                        display_name: so.display_name,
                        outcome: so.outcome,
                        active_secs: so.active_secs,
                    },
                    commits,
                    decisions,
                    research,
                });
            }
            out
        }));
    }

    // Pre-seed EVERY calendar day in the window (oldest-first) so the grid
    // is always exactly `days` entries — a day with no captures is empty,
    // not absent (the daycard determinism contract).
    type DayAcc = (
        Vec<(i64, LedgerSessionOut)>,
        Vec<LedgerCommitOut>,
        u32,
        Vec<String>,
    );
    let mut by_date: std::collections::BTreeMap<String, DayAcc> = std::collections::BTreeMap::new();
    for i in 0..days {
        let d = (start_date + chrono::Duration::days(i as i64)).to_string();
        by_date.insert(d, (Vec::new(), Vec::new(), 0, Vec::new()));
    }

    // PF-R1 — the operator-configurable `[server] fanout_cap` (default 8,
    // byte-identical to the old hardcoded `super::FANOUT_CAP`).
    for row in super::buffered_join(futs, state.fanout_cap)
        .await
        .into_iter()
        .flatten()
    {
        // A row's date can fall outside the pre-seeded grid only via clock
        // skew between `window_start`'s UTC-midnight truncation and a
        // session's own `started_at` — never in practice, but `entry(..).
        // or_default()` keeps this total rather than silently dropping data.
        let entry = by_date.entry(row.date).or_default();
        entry.1.extend(row.commits);
        entry.2 += row.decisions;
        entry.3.extend(row.research);
        entry.0.push((row.started_at, row.session));
    }

    let mut totals = LedgerTotalsOut::default();
    let mut days_out = Vec::with_capacity(by_date.len());
    for (date, (mut sess_rows, commits, decisions_count, research)) in by_date {
        sess_rows.sort_by_key(|(started_at, _)| std::cmp::Reverse(*started_at)); // newest-first
        totals.sessions += sess_rows.len() as u32;
        totals.commits += commits.len() as u32;
        totals.decisions += decisions_count;
        totals.active_secs += sess_rows.iter().map(|(_, s)| s.active_secs).sum::<i64>();
        days_out.push(LedgerDayOut {
            date,
            sessions: sess_rows.into_iter().map(|(_, s)| s).collect(),
            commits,
            decisions_count,
            research_topics: top_n_by_frequency(research, LEDGER_TOPICS_PER_DAY),
        });
    }

    Json(LedgerResponse {
        project: p.project,
        days,
        days_out,
        totals,
    })
}

/// Rank `items` by frequency (desc), ties broken alphabetically —
/// deterministic (#10) — and keep the top `n` distinct values.
fn top_n_by_frequency(items: Vec<String>, n: usize) -> Vec<String> {
    let mut counts: std::collections::BTreeMap<String, u32> = std::collections::BTreeMap::new();
    for item in items {
        *counts.entry(item).or_insert(0) += 1;
    }
    let mut ranked: Vec<(String, u32)> = counts.into_iter().collect();
    ranked.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    ranked.into_iter().take(n).map(|(k, _)| k).collect()
}

#[cfg(test)]
mod ledger_tests {
    use super::top_n_by_frequency;

    // W6 (moonshots M4) — `routes/sessions.rs` has no existing test harness
    // (every other aggregate route here — funnel/research_rollup/threads —
    // is validated only via e2e, consistent with the rest of this file); the
    // one genuinely pure, easily-unit-tested piece of the ledger route is
    // its research-topics ranking, so that's what gets covered here rather
    // than introducing a bespoke axum-state test harness for one route.
    #[test]
    fn ranks_by_frequency_then_alphabetically() {
        let items = vec!["b", "a", "b", "c", "a", "a"]
            .into_iter()
            .map(String::from)
            .collect();
        // a:3, b:2, c:1 — top 2 = [a, b].
        assert_eq!(
            top_n_by_frequency(items, 2),
            vec!["a".to_string(), "b".to_string()]
        );
    }

    #[test]
    fn ties_break_alphabetically_deterministic() {
        let items = vec!["z", "a", "m"].into_iter().map(String::from).collect();
        assert_eq!(
            top_n_by_frequency(items, 3),
            vec!["a".to_string(), "m".to_string(), "z".to_string()]
        );
    }

    #[test]
    fn caps_at_n_even_with_more_distinct_values() {
        let items = vec!["a", "b", "c", "d"]
            .into_iter()
            .map(String::from)
            .collect();
        assert_eq!(top_n_by_frequency(items, 3).len(), 3);
    }
}

/// A narrative thread (P7) — a run of sessions in one folder that form a
/// continued effort (no gap larger than `THREAD_GAP_SECS`). Read-only +
/// deterministic; auto-clustered, a suggestion not a stored entity.
#[cfg_attr(feature = "ts-export", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Serialize)]
pub struct ThreadOut {
    /// Full working-directory path.
    pub folder: String,
    /// Display label (basename of `folder`).
    pub label: String,
    /// Representative title — the newest session's display name.
    pub title: String,
    pub count: u64,
    /// Span: earliest start … latest end (unix secs).
    pub started_at: i64,
    pub ended_at: i64,
    /// The thread's sessions, newest-first.
    pub sessions: Vec<SessionOut>,
}

#[cfg_attr(feature = "ts-export", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Serialize)]
pub struct ThreadsResponse {
    pub threads: Vec<ThreadOut>,
}

/// Upper bound on sessions scanned per kb when clustering threads. Generous;
/// a corpus with more sessions than this simply clusters its newest window.
const THREAD_SCAN_CAP: u32 = 1000;

/// `GET /api/sessions/threads` — sessions clustered into continued-effort
/// threads (P7): grouped by folder, then split on time gaps. Newest thread
/// first. Deterministic, LLM-free.
pub async fn threads(State(state): State<Arc<KbHandles>>) -> impl IntoResponse {
    // Gather all sessions (bounded), as SessionOut, across corpora.
    let mut futs: Vec<super::CorpusFut<'_, Vec<SessionOut>>> = Vec::new();
    for (kb_name, ctx) in state.kbs.iter() {
        futs.push(Box::pin(async move {
            ctx.storage
                .sessions_list(
                    THREAD_SCAN_CAP,
                    None,
                    None,
                    None,
                    None,
                    Default::default(),
                    Vec::new(),
                    Vec::new(),
                )
                .await
                .map(|rows| {
                    rows.into_iter()
                        .map(|r| SessionOut::from_row(kb_name.as_str(), r))
                        .collect::<Vec<_>>()
                })
                .unwrap_or_else(|e| {
                    tracing::warn!(kb = %kb_name, error = %e, "sessions_list failed in threads");
                    Vec::new()
                })
        }));
    }
    // PF-R1 — the operator-configurable `[server] fanout_cap` (default 8,
    // byte-identical to the old hardcoded `super::FANOUT_CAP`).
    let all: Vec<SessionOut> = super::buffered_join(futs, state.fanout_cap)
        .await
        .into_iter()
        .flatten()
        .collect();
    Json(ThreadsResponse {
        threads: cluster_threads(all),
    })
}

/// Cluster sessions into threads: group by `cwd`, sort each group ascending,
/// split on gaps via `kb_core::sessions::thread_boundaries`, emit newest-first.
fn cluster_threads(sessions: Vec<SessionOut>) -> Vec<ThreadOut> {
    let mut by_folder: std::collections::BTreeMap<String, Vec<SessionOut>> =
        std::collections::BTreeMap::new();
    for s in sessions {
        if let Some(cwd) = s.cwd.clone() {
            by_folder.entry(cwd).or_default().push(s);
        }
    }
    let mut threads: Vec<ThreadOut> = Vec::new();
    for (folder, mut group) in by_folder {
        group.sort_by_key(|s| s.started_at); // ascending for the gap walk
        let spans: Vec<(i64, i64)> = group.iter().map(|s| (s.started_at, s.ended_at)).collect();
        let bounds: std::collections::BTreeSet<usize> =
            kb_core::sessions::thread_boundaries(&spans, kb_core::sessions::THREAD_GAP_SECS)
                .into_iter()
                .collect();
        // Walk the ascending group, starting a new run at each boundary index.
        let mut run: Vec<SessionOut> = Vec::new();
        for (i, s) in group.into_iter().enumerate() {
            if bounds.contains(&i) && !run.is_empty() {
                threads.push(make_thread(&folder, std::mem::take(&mut run)));
            }
            run.push(s);
        }
        if !run.is_empty() {
            threads.push(make_thread(&folder, run));
        }
    }
    threads.sort_by(|a, b| {
        b.ended_at
            .cmp(&a.ended_at)
            .then_with(|| a.folder.cmp(&b.folder))
    });
    threads
}

/// Build a `ThreadOut` from one ascending run of sessions. Reverses to
/// newest-first; the title is the newest session's display name.
fn make_thread(folder: &str, mut run: Vec<SessionOut>) -> ThreadOut {
    let started_at = run.iter().map(|s| s.started_at).min().unwrap_or(0);
    let ended_at = run.iter().map(|s| s.ended_at).max().unwrap_or(0);
    let label = folder
        .trim_end_matches('/')
        .rsplit('/')
        .next()
        .unwrap_or(folder)
        .to_string();
    run.reverse(); // newest-first
    let title = run
        .first()
        .map(|s| s.display_name.clone())
        .unwrap_or_default();
    let count = run.len() as u64;
    ThreadOut {
        folder: folder.to_string(),
        label,
        title,
        count,
        started_at,
        ended_at,
        sessions: run,
    }
}

/// P8 — body for materialising a thread into an editable kb-list/1 list.
#[derive(Debug, Deserialize)]
pub struct SaveThreadBody {
    /// The corpus the sessions live in (all of a thread's sessions share one).
    pub kb: String,
    /// The list title (the SPA passes the thread's representative title).
    pub title: String,
    /// The session artifact ids, in display order.
    pub artifact_ids: Vec<String>,
    /// CT-E5 — expand each session into its STORY instead of one entry per
    /// transcript: capture → files touched → memories produced → memories
    /// recalled (`kb_core::sessions::narrative`). **Opt-in**: the flat
    /// one-entry-per-session shape is the pinned pre-CT-E5 contract, so an
    /// existing caller that doesn't ask keeps exactly what it had.
    #[serde(default)]
    pub narrative: bool,
}

#[cfg_attr(feature = "ts-export", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Serialize)]
pub struct SaveThreadResponse {
    pub kb: String,
    pub list_id: String,
}

/// CT-E5 — read ONE session's four narrative lanes off storage and hand them
/// to the pure orderer. Every read here is an EXISTING per-session read,
/// already newest-capture scoped (invariant #11); nothing new is queried and
/// nothing is cached.
///
/// * capture — the transcript artifact the caller named;
/// * touched — `session_files_for_session` (edits before reads, the manifest
///   order the `/sessions/{sid}/files` route already serves);
/// * produced — `list_docs_with_kb_session` (the `/sessions/{sid}/memories`
///   read), sorted OLDEST-first (the memories route sorts newest-first for a
///   feed; a story runs forwards) with an id tiebreak so the order is total;
/// * recalled — `memory_recalls_for_session` (the `/sessions/{sid}/recalls`
///   read), ledger order, oldest first.
///
/// The produced/recalled reads are deliberately scoped to THIS kb only
/// (`ctx`), not fanned out over `state.kbs`: a reading list is single-corpus
/// (`routes::lists::enrich_artifacts` resolves every entry against one
/// `KbContext`), so a foreign-corpus id could only ever render as a
/// tombstone. `narrative_entries` counts what that drops.
async fn session_lanes(
    ctx: &crate::state::KbContext,
    kb: &str,
    session_id: &str,
    capture_artifact_id: &str,
) -> kb_core::sessions::narrative::SessionLanes {
    use kb_core::sessions::narrative::{LaneItem, SessionLanes};

    let touched: Vec<LaneItem> = ctx
        .storage
        .session_files_for_session(session_id.to_string())
        .await
        .unwrap_or_else(|e| {
            tracing::warn!(kb = %kb, session_id = %session_id, error = %e,
                "session_files_for_session failed in narrative save");
            Vec::new()
        })
        .into_iter()
        .filter(|r| r.in_corpus)
        .filter_map(|r| {
            let id = r.target_artifact_id?;
            // `target_kb` is the row's own resolution; absent ⇒ treat it as
            // this kb (the pre-V0026 shape), which the pure orderer then
            // filters against the list's kb anyway.
            let row_kb = r.target_kb.unwrap_or_else(|| kb.to_string());
            Some(LaneItem::with_detail(row_kb, id, r.action))
        })
        .collect();

    let mut produced_docs = ctx
        .storage
        .list_docs_with_kb_session(session_id.to_string(), PER_SESSION_MEMORY_LIMIT)
        .await
        .unwrap_or_else(|e| {
            tracing::warn!(kb = %kb, session_id = %session_id, error = %e,
                "list_docs_with_kb_session failed in narrative save");
            Vec::new()
        });
    produced_docs.sort_by(|a, b| {
        a.mtime_unix
            .unwrap_or(i64::MAX)
            .cmp(&b.mtime_unix.unwrap_or(i64::MAX))
            .then_with(|| a.id.cmp(&b.id))
    });
    let produced: Vec<LaneItem> = produced_docs
        .into_iter()
        .map(|d| LaneItem::new(kb, d.id))
        .collect();

    let recalled: Vec<LaneItem> = ctx
        .storage
        .memory_recalls_for_session(session_id.to_string())
        .await
        .unwrap_or_else(|e| {
            tracing::warn!(kb = %kb, session_id = %session_id, error = %e,
                "memory_recalls_for_session failed in narrative save");
            Vec::new()
        })
        .into_iter()
        .map(|r| {
            // CT-C5 `used` is SURFACED, never scored — it only decorates the
            // note here; it changes no ordering.
            if r.used {
                LaneItem::with_detail(r.memory_kb, r.memory_id, "used")
            } else {
                LaneItem::new(r.memory_kb, r.memory_id)
            }
        })
        .collect();

    SessionLanes {
        capture: Some(LaneItem::new(kb, capture_artifact_id)),
        touched,
        produced,
        recalled,
    }
}

/// `POST /api/sessions/threads/save` — materialise a thread into an editable
/// kb-list/1 list (P8): create the list, then import the thread's sessions as
/// entries (one tx). The entries target session transcripts, so the lists view
/// suppresses their read-state (invariant #25, via `ListEntry::is_session`).
///
/// **CT-E5** — with `"narrative": true` the list is the session's STORY
/// instead of one entry per transcript: for each session, in the caller's
/// order, capture → files touched (edits before reads) → memories produced →
/// memories recalled, each entry noting its lane. The description carries the
/// ordering contract, each session's id + capture date, and — only when the
/// kb has an inert `[kb.*] code_url` configured — the kb-code session-diff
/// link (a rendered link; kb never calls kb-code, invariant #2). Default
/// `false` keeps the pre-CT-E5 flat shape, which has a pinned contract test.
pub async fn save_thread(
    State(state): State<Arc<KbHandles>>,
    axum::extract::Extension(identity): axum::extract::Extension<crate::middleware::Identity>,
    Json(body): Json<SaveThreadBody>,
) -> axum::response::Response {
    use axum::response::IntoResponse;
    use kb_core::sessions::narrative;
    let (kb_name, ctx) = match crate::routes::resolve_kb(&state, &body.kb) {
        Ok(v) => v,
        Err(resp) => return resp,
    };
    let title = body.title.trim().to_string();
    if title.is_empty() || body.artifact_ids.is_empty() {
        return error_to_problem_json(&kb_core::Error::BadRequest(
            "thread save needs a title and at least one session".into(),
        ));
    }
    if body.narrative && body.artifact_ids.len() > narrative::SESSIONS_PER_SAVE_CAP {
        // Explicit over-cap refusal (the `?ids=` precedent, #35): never a
        // silently truncated story.
        return error_to_problem_json(&kb_core::Error::BadRequest(format!(
            "a narrative save expands at most {} sessions; got {}",
            narrative::SESSIONS_PER_SAVE_CAP,
            body.artifact_ids.len()
        )));
    }

    // CT-E5 — resolve the narrative entries + description BEFORE creating the
    // list, so a storage failure can't leave a titled-but-empty list behind.
    let mut narrative_plan: Vec<(String, Option<String>)> = Vec::new();
    let mut description: Option<String> = None;
    if body.narrative {
        let rows = match ctx
            .storage
            .sessions_get_by_artifact_ids(body.artifact_ids.clone())
            .await
        {
            Ok(r) => r,
            Err(e) => return error_to_problem_json(&e),
        };
        let by_artifact: std::collections::HashMap<String, kb_core::storage::sqlite::SessionRow> =
            rows.into_iter()
                .map(|r| (r.artifact_id.clone(), r))
                .collect();
        let mut stamps: Vec<narrative::SessionStamp> = Vec::new();
        let mut planned_ids: std::collections::HashSet<String> = std::collections::HashSet::new();
        let mut skipped = 0usize;
        // Serial over the thread's sessions — this is a bounded write path,
        // not a `state.kbs` fan-out (#28 governs the latter).
        for aid in &body.artifact_ids {
            let Some(row) = by_artifact.get(aid) else {
                // The caller named an artifact this kb has no session row
                // for; nothing to tell about it, so no entry — honest over a
                // phantom "capture" pointing at a non-session.
                continue;
            };
            stamps.push(narrative::SessionStamp {
                session_id: row.session_id.clone(),
                started_at: row.started_at,
            });
            let lanes = session_lanes(ctx, kb_name.as_str(), &row.session_id, aid).await;
            let (entries, dropped) = narrative::narrative_entries(
                &lanes,
                kb_name.as_str(),
                narrative::ENTRIES_PER_SESSION_CAP,
            );
            skipped += dropped;
            for e in entries {
                // Across a multi-session thread the same artifact can recur
                // (one file touched twice). First telling wins — the import
                // tx would dedupe on `(artifact_id, anchor)` anyway; doing it
                // here keeps `skipped` truthful.
                if planned_ids.insert(e.artifact_id.clone()) {
                    narrative_plan.push((e.artifact_id, Some(e.note)));
                }
            }
        }
        // Every entry must name an artifact that still resolves in THIS kb —
        // the same rule `routes::lists::resolve_target` enforces on a manual
        // add ("refusing unknown targets keeps deliberate tombstones out").
        // A `memory_recalls` row whose memory was hard-deleted is real
        // history but a dead list entry, so it is counted, not listed.
        if !narrative_plan.is_empty() {
            let ids: Vec<String> = narrative_plan.iter().map(|(id, _)| id.clone()).collect();
            let resolvable: std::collections::HashSet<String> = ctx
                .storage
                .get_by_ids(ids)
                .await
                .unwrap_or_default()
                .into_iter()
                .map(|d| d.id)
                .collect();
            let before = narrative_plan.len();
            narrative_plan.retain(|(id, _)| resolvable.contains(id));
            skipped += before - narrative_plan.len();
        }
        if narrative_plan.is_empty() {
            return error_to_problem_json(&kb_core::Error::BadRequest(
                "narrative save found no resolvable artifacts for those session ids".into(),
            ));
        }
        description = Some(narrative::narrative_description(
            &stamps,
            ctx.code_url.as_deref(),
            skipped,
        ));
    }

    let now = chrono::Utc::now().timestamp();
    let list = match ctx
        .storage
        .list_create(
            kb_core::lists::new_list_id(),
            title,
            description,
            false,
            now,
        )
        .await
    {
        Ok(l) => l,
        Err(e) => return error_to_problem_json(&e),
    };
    let planned: Vec<(String, Option<String>)> = if body.narrative {
        narrative_plan
    } else {
        body.artifact_ids
            .iter()
            .map(|aid| (aid.clone(), None))
            .collect()
    };
    let entries: Vec<kb_core::lists::NewListEntry> = planned
        .into_iter()
        .map(|(artifact_id, note)| kb_core::lists::NewListEntry {
            id: kb_core::lists::new_entry_id(),
            list_id: list.id.clone(),
            kb: kb_name.as_str().to_string(),
            artifact_id,
            anchor_json: None,
            note,
            words: None,
            read_override: None,
        })
        .collect();
    if let Err(e) = ctx
        .storage
        .list_import_entries(
            list.id.clone(),
            kb_core::lists::ImportMode::Append,
            entries,
            identity.user.clone(),
            now,
        )
        .await
    {
        return error_to_problem_json(&e);
    }
    ctx.bus.emit(
        "list.created",
        serde_json::json!({ "kb": kb_name.as_str(), "id": list.id, "title": list.title }),
    );
    Json(SaveThreadResponse {
        kb: kb_name.as_str().to_string(),
        list_id: list.id,
    })
    .into_response()
}

/// One file a session touched (A4/A6) — the wire shape of a `session_files`
/// row, with the in-corpus artifact resolved to a link target + title.
#[cfg_attr(feature = "ts-export", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Serialize)]
pub struct SessionFileOut {
    /// Verbatim path from the transcript.
    pub path: String,
    /// Final path component (display + grouping).
    pub basename: String,
    /// `"read" | "write" | "edit"`.
    pub action: String,
    /// True when the path resolved under a kb mount (links to an artifact).
    pub in_corpus: bool,
    /// Resolved corpus name, when in-corpus.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts-export", ts(optional))]
    pub kb: Option<String>,
    /// Resolved artifact id, when in-corpus.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts-export", ts(optional))]
    pub target_artifact_id: Option<String>,
    /// Source-relative path of the target artifact (for the SPA `<Link>`),
    /// resolved only when the artifact still exists in lance.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts-export", ts(optional))]
    pub source_relative: Option<String>,
    /// Target artifact title, when resolvable.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts-export", ts(optional))]
    pub title: Option<String>,
    /// V0026/W0.5 — true when this touch was recovered from a subagent's own
    /// sidecar transcript rather than the main thread.
    pub via_subagent: bool,
}

#[cfg_attr(feature = "ts-export", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Serialize)]
pub struct SessionFilesResponse {
    pub files: Vec<SessionFileOut>,
}

/// `GET /api/sessions/{session_id}/files` — the file-activity manifest for a
/// session (A4/A6): every file it read/edited/wrote, with in-corpus files
/// resolved to a link target + title and out-of-corpus files kept as plain
/// paths. Cheap indexed read of `session_files` (no transcript re-scan).
pub async fn files(
    State(state): State<Arc<KbHandles>>,
    Path(session_id): Path<String>,
) -> impl IntoResponse {
    let session_id = &session_id;
    // The session lives in one kb; fan out the lookup, others return empty.
    let mut futs: Vec<super::CorpusFut<'_, Vec<kb_core::storage::sqlite::SessionFileRow>>> =
        Vec::new();
    for (kb_name, ctx) in state.kbs.iter() {
        futs.push(Box::pin(async move {
            ctx.storage
                .session_files_for_session(session_id.clone())
                .await
                .unwrap_or_else(|e| {
                    tracing::warn!(kb = %kb_name, error = %e, "session_files_for_session failed");
                    Vec::new()
                })
        }));
    }
    // PF-R1 — the operator-configurable `[server] fanout_cap` (default 8,
    // byte-identical to the old hardcoded `super::FANOUT_CAP`).
    let rows: Vec<kb_core::storage::sqlite::SessionFileRow> =
        super::buffered_join(futs, state.fanout_cap)
            .await
            .into_iter()
            .flatten()
            .collect();

    // Resolve title + source_relative for in-corpus rows whose target still
    // exists in lance. Group targets by kb → one `get_by_ids` per kb (PSRV-7)
    // rather than one round-trip per row; fan out the per-kb batches through
    // buffered_join (submission-ordered, #28) and map results back by id.
    let mut files: Vec<SessionFileOut> = rows
        .iter()
        .map(|row| SessionFileOut {
            path: row.path.clone(),
            basename: row.basename.clone(),
            action: row.action.clone(),
            in_corpus: row.in_corpus,
            kb: row.target_kb.clone(),
            target_artifact_id: row.target_artifact_id.clone(),
            source_relative: None,
            title: None,
            via_subagent: row.via_subagent,
        })
        .collect();

    // Collect distinct target ids per kb (submission-order over state.kbs).
    // Per-kb batch result: kb name → { artifact id → (source_relative, title) }.
    type ResolvedTitles = std::collections::HashMap<String, (String, String)>;
    let mut resolve_futs: Vec<super::CorpusFut<'_, (String, ResolvedTitles)>> = Vec::new();
    for (kb_name, ctx) in state.kbs.iter() {
        let mut ids: Vec<String> = rows
            .iter()
            .filter(|r| r.target_kb.as_deref() == Some(kb_name.as_str()))
            .filter_map(|r| r.target_artifact_id.clone())
            .collect();
        ids.sort();
        ids.dedup();
        if ids.is_empty() {
            continue;
        }
        let kb_owned = kb_name.as_str().to_string();
        let source_path = ctx.source_path.clone();
        resolve_futs.push(Box::pin(async move {
            let docs = ctx.storage.get_by_ids(ids).await.unwrap_or_else(|e| {
                tracing::warn!(kb = %kb_owned, error = %e, "get_by_ids failed in session files");
                Vec::new()
            });
            let mut map = std::collections::HashMap::with_capacity(docs.len());
            for doc in docs {
                let rel = kb_core::paths::doc_rel_path(&doc.path, &source_path);
                map.insert(doc.id, (rel, doc.title));
            }
            (kb_owned, map)
        }));
    }
    let mut resolved: std::collections::HashMap<(String, String), (String, String)> =
        std::collections::HashMap::new();
    // PF-R1 — the operator-configurable `[server] fanout_cap` (default 8,
    // byte-identical to the old hardcoded `super::FANOUT_CAP`).
    for (kb, map) in super::buffered_join(resolve_futs, state.fanout_cap).await {
        for (aid, val) in map {
            resolved.insert((kb.clone(), aid), val);
        }
    }
    for out in &mut files {
        let Some(kb) = out.kb.as_ref() else {
            continue;
        };
        let Some(aid) = out.target_artifact_id.as_ref() else {
            continue;
        };
        if let Some((source_relative, title)) = resolved.get(&(kb.clone(), aid.clone())) {
            out.source_relative = Some(source_relative.clone());
            out.title = Some(title.clone());
        }
    }
    // Edits first, then by basename, for a stable manifest.
    files.sort_by(|a, b| {
        action_rank(&b.action)
            .cmp(&action_rank(&a.action))
            .then_with(|| a.basename.cmp(&b.basename))
    });
    Json(SessionFilesResponse { files })
}

/// One steering decision (S9) — an AskUserQuestion answer or plan approval.
#[cfg_attr(feature = "ts-export", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Serialize)]
pub struct DecisionOut {
    /// `"question" | "plan"`.
    pub kind: String,
    pub prompt: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts-export", ts(optional))]
    pub answer: Option<String>,
}

#[cfg_attr(feature = "ts-export", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Serialize)]
pub struct SessionDecisionsResponse {
    pub decisions: Vec<DecisionOut>,
}

/// `GET /api/sessions/{session_id}/decisions` — the per-session decisions log
/// (S9): every steering moment in order. Cheap indexed read.
pub async fn decisions(
    State(state): State<Arc<KbHandles>>,
    Path(session_id): Path<String>,
) -> impl IntoResponse {
    let session_id = &session_id;
    let mut futs: Vec<super::CorpusFut<'_, Vec<kb_core::storage::sqlite::SessionDecisionRow>>> =
        Vec::new();
    for (kb_name, ctx) in state.kbs.iter() {
        futs.push(Box::pin(async move {
            ctx.storage
                .session_decisions_for_session(session_id.clone())
                .await
                .unwrap_or_else(|e| {
                    tracing::warn!(kb = %kb_name, error = %e, "session_decisions_for_session failed");
                    Vec::new()
                })
        }));
    }
    // PF-R1 — the operator-configurable `[server] fanout_cap` (default 8,
    // byte-identical to the old hardcoded `super::FANOUT_CAP`).
    let mut rows: Vec<kb_core::storage::sqlite::SessionDecisionRow> =
        super::buffered_join(futs, state.fanout_cap)
            .await
            .into_iter()
            .flatten()
            .collect();
    rows.sort_by_key(|r| r.seq);
    let decisions = rows
        .into_iter()
        .map(|r| DecisionOut {
            kind: r.kind,
            prompt: r.prompt,
            answer: r.answer,
        })
        .collect();
    Json(SessionDecisionsResponse { decisions })
}

/// One git action a session produced (P5). V0025/W0.4 adds the capture-time
/// git resolution fields (`sha_full`/`repo_root`/`author`/`parents`/
/// `trailers`, `resolved` flags whether they were populated) — additive +
/// optional, `None`/empty on every pre-W0.4 row.
#[cfg_attr(feature = "ts-export", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Serialize)]
pub struct CommitOut {
    /// `"commit" | "push" | "tag"`.
    pub kind: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts-export", ts(optional))]
    pub sha: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts-export", ts(optional))]
    pub subject: Option<String>,
    /// Did `kb sessions capture`'s `git show -s` resolve this sha.
    pub resolved: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts-export", ts(optional))]
    pub sha_full: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts-export", ts(optional))]
    pub repo_root: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts-export", ts(optional))]
    pub author: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts-export", ts(optional))]
    pub parents: Option<i64>,
    /// Trailer lines (`"Key: value"`), split from the DB's newline-joined
    /// storage — includes `Kb-Session` when present. Empty when resolution
    /// found none or never ran.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub trailers: Vec<String>,
}

impl From<kb_core::storage::sqlite::SessionCommitRow> for CommitOut {
    fn from(r: kb_core::storage::sqlite::SessionCommitRow) -> Self {
        CommitOut {
            kind: r.kind,
            sha: r.sha,
            subject: r.subject,
            resolved: r.resolved,
            sha_full: r.sha_full,
            repo_root: r.repo_root,
            author: r.author,
            parents: r.parents,
            trailers: r
                .trailers
                .map(|t| t.lines().map(str::to_string).collect())
                .unwrap_or_default(),
        }
    }
}

#[cfg_attr(feature = "ts-export", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Serialize)]
pub struct SessionCommitsResponse {
    pub commits: Vec<CommitOut>,
}

/// `GET /api/sessions/{session_id}/commits` — the git actions a session
/// produced (P5), in order. "What shipped from this work."
pub async fn commits(
    State(state): State<Arc<KbHandles>>,
    Path(session_id): Path<String>,
) -> impl IntoResponse {
    let session_id = &session_id;
    let mut futs: Vec<super::CorpusFut<'_, Vec<kb_core::storage::sqlite::SessionCommitRow>>> =
        Vec::new();
    for (kb_name, ctx) in state.kbs.iter() {
        futs.push(Box::pin(async move {
            ctx.storage
                .session_commits_for_session(session_id.clone())
                .await
                .unwrap_or_else(|e| {
                    tracing::warn!(kb = %kb_name, error = %e, "session_commits_for_session failed");
                    Vec::new()
                })
        }));
    }
    // PF-R1 — the operator-configurable `[server] fanout_cap` (default 8,
    // byte-identical to the old hardcoded `super::FANOUT_CAP`).
    let mut rows: Vec<kb_core::storage::sqlite::SessionCommitRow> =
        super::buffered_join(futs, state.fanout_cap)
            .await
            .into_iter()
            .flatten()
            .collect();
    rows.sort_by_key(|r| r.seq);
    let commits = rows.into_iter().map(CommitOut::from).collect();
    Json(SessionCommitsResponse { commits })
}

// ---- MI-W4.6 — provenance thread: per-commit touched-file staleness ------
//
// The 3rd hop of the memory→session→commits→"has this changed since" thread
// (routes/memory.rs's provenance section, SPA side). A cheap, bounded,
// on-demand git read — `kb_core::vcs::commit_touched_files` does the actual
// shelling; this route just resolves WHICH repo_root/sha_full to run it
// against, off the session's own `session_commits` rows. No new storage.

/// One file `GET .../commits/{sha}/files` resolved staleness for.
#[cfg_attr(feature = "ts-export", derive(ts_rs::TS), ts(export, optional_fields))]
#[derive(Debug, Serialize)]
pub struct TouchedFileOut {
    pub path: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_touched_unix: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub changed_since: Option<bool>,
}

impl From<kb_core::vcs::TouchedFile> for TouchedFileOut {
    fn from(f: kb_core::vcs::TouchedFile) -> Self {
        TouchedFileOut {
            path: f.path,
            last_touched_unix: f.last_touched_unix,
            changed_since: f.changed_since,
        }
    }
}

#[cfg_attr(feature = "ts-export", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Serialize)]
pub struct CommitFilesResponse {
    /// `false` when the matching `session_commits` row is missing, or its
    /// capture-time resolution never ran/succeeded (no `repo_root`/
    /// `sha_full` to run git against) — the caller renders "unknown" rather
    /// than an empty-but-confident list. Never a 404: the session/commit
    /// pairing can be perfectly valid while only the git lookup is
    /// unavailable (a deleted worktree, an unresolvable sha).
    pub available: bool,
    pub files: Vec<TouchedFileOut>,
    /// `true` when the commit touched more files than
    /// `kb_core::vcs::TOUCHED_FILES_CAP` — only the first N were resolved.
    pub truncated: bool,
}

fn commit_files_unavailable() -> Json<CommitFilesResponse> {
    Json(CommitFilesResponse {
        available: false,
        files: Vec::new(),
        truncated: false,
    })
}

/// `GET /api/sessions/{session_id}/commits/{sha}/files` — the files `sha`
/// (one of `session_id`'s recorded commits) touched, each annotated with
/// whether it's changed again since. Fans out across every kb exactly like
/// `commits` above (a session's commits can be recorded from any kb that
/// captured it); `sha` matches either the full or transcript-detected short
/// form recorded on the row.
pub async fn commit_files(
    State(state): State<Arc<KbHandles>>,
    Path((session_id, sha)): Path<(String, String)>,
) -> Json<CommitFilesResponse> {
    let mut futs: Vec<super::CorpusFut<'_, Vec<kb_core::storage::sqlite::SessionCommitRow>>> =
        Vec::new();
    for (kb_name, ctx) in state.kbs.iter() {
        let session_id = session_id.clone();
        futs.push(Box::pin(async move {
            ctx.storage
                .session_commits_for_session(session_id)
                .await
                .unwrap_or_else(|e| {
                    tracing::warn!(kb = %kb_name, error = %e, "session_commits_for_session failed");
                    Vec::new()
                })
        }));
    }
    // PF-R1 — the operator-configurable `[server] fanout_cap` (default 8,
    // byte-identical to the old hardcoded `super::FANOUT_CAP`).
    let rows: Vec<kb_core::storage::sqlite::SessionCommitRow> =
        super::buffered_join(futs, state.fanout_cap)
            .await
            .into_iter()
            .flatten()
            .collect();

    let matched = rows.into_iter().find(|r| {
        r.sha.as_deref() == Some(sha.as_str()) || r.sha_full.as_deref() == Some(sha.as_str())
    });
    let Some(matched) = matched.filter(|r| r.resolved) else {
        return commit_files_unavailable();
    };
    let (Some(repo_root), Some(sha_full)) = (matched.repo_root, matched.sha_full) else {
        return commit_files_unavailable();
    };

    let (files, truncated) = kb_core::vcs::commit_touched_files(
        std::path::Path::new(&repo_root),
        &sha_full,
        kb_core::vcs::TOUCHED_FILES_CAP,
    )
    .await;
    Json(CommitFilesResponse {
        available: true,
        files: files.into_iter().map(TouchedFileOut::from).collect(),
        truncated,
    })
}

// ---- W0.6 — sha→session reverse lookup + the bulk commit-map feed --------
//
// kb-code Wave 0's wedge instrument: given a git sha, which session produced
// it (`by_commit`); and a flat, paginated feed of every recorded commit for
// wave-3's offline join precomputation (`commit_map`). Both are
// newest-capture-scoped (#11) and cross-kb fanned out (#28).

/// One `session_commits` match for `GET /api/sessions/by-commit?sha=`.
#[cfg_attr(feature = "ts-export", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Serialize)]
pub struct CommitMatchOut {
    pub kb: String,
    pub session_id: String,
    /// The session's NEWEST-capture artifact id (the row this match came
    /// from — #11).
    pub artifact_id: String,
    /// `"commit" | "push" | "tag"`.
    pub kind: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts-export", ts(optional))]
    pub sha: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts-export", ts(optional))]
    pub sha_full: Option<String>,
    pub resolved: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts-export", ts(optional))]
    pub subject: Option<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub trailers: Vec<String>,
    /// Server-computed display name (title → first_user_prompt → short id —
    /// the same ladder as [`SessionOut::from_row`]).
    pub display_name: String,
    pub started_at: i64,
}

#[cfg_attr(feature = "ts-export", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Serialize)]
pub struct SessionsByCommitResponse {
    pub matches: Vec<CommitMatchOut>,
}

#[derive(Debug, Deserialize)]
pub struct ByCommitParams {
    pub sha: String,
}

/// `GET /api/sessions/by-commit?sha=<sha-or-prefix>` (kb-code Wave 0 / W0.6)
/// — the sha→session reverse lookup: every `session_commits` row across every
/// corpus whose `sha` OR `sha_full` (V0025) starts with the given prefix
/// (>=7 hex chars — shorter is 400, matching git's own abbreviation floor),
/// newest-capture-scoped (#11), cross-kb fanned out (#28). Newest-first.
pub async fn by_commit(
    State(state): State<Arc<KbHandles>>,
    Query(params): Query<ByCommitParams>,
) -> Response {
    let prefix = params.sha.trim().to_ascii_lowercase();
    if prefix.chars().count() < BY_COMMIT_MIN_PREFIX
        || !prefix.bytes().all(|b| b.is_ascii_hexdigit())
    {
        return error_to_problem_json(&kb_core::Error::BadRequest(format!(
            "sha must be at least {BY_COMMIT_MIN_PREFIX} hex characters (got {:?})",
            params.sha
        )));
    }
    let prefix = &prefix;
    let mut futs: Vec<super::CorpusFut<'_, Vec<CommitMatchOut>>> = Vec::new();
    for (kb_name, ctx) in state.kbs.iter() {
        futs.push(Box::pin(async move {
            let rows = match ctx
                .storage
                .session_commits_by_sha_prefix(prefix.clone())
                .await
            {
                Ok(r) => r,
                Err(e) => {
                    tracing::warn!(kb = %kb_name, error = %e, "session_commits_by_sha_prefix failed");
                    return Vec::new();
                }
            };
            rows.into_iter()
                .map(|m| {
                    let display_name = display_name_of(
                        m.title.as_deref(),
                        m.first_user_prompt.as_deref(),
                        &m.commit.session_id,
                    );
                    CommitMatchOut {
                        kb: kb_name.as_str().to_string(),
                        session_id: m.commit.session_id,
                        artifact_id: m.commit.artifact_id_session,
                        kind: m.commit.kind,
                        sha: m.commit.sha,
                        sha_full: m.commit.sha_full,
                        resolved: m.commit.resolved,
                        subject: m.commit.subject,
                        trailers: m
                            .commit
                            .trailers
                            .map(|t| t.lines().map(str::to_string).collect())
                            .unwrap_or_default(),
                        display_name,
                        started_at: m.started_at,
                    }
                })
                .collect()
        }));
    }
    // PF-R1 — the operator-configurable `[server] fanout_cap` (default 8,
    // byte-identical to the old hardcoded `super::FANOUT_CAP`).
    let mut matches: Vec<CommitMatchOut> = super::buffered_join(futs, state.fanout_cap)
        .await
        .into_iter()
        .flatten()
        .collect();
    matches.sort_by(|a, b| {
        b.started_at
            .cmp(&a.started_at)
            .then_with(|| a.kb.cmp(&b.kb))
            .then_with(|| a.session_id.cmp(&b.session_id))
    });
    Json(SessionsByCommitResponse { matches }).into_response()
}

/// One row of the `GET /api/sessions/commit-map` bulk feed: every
/// `session_commits` column, plus `kb`/`artifact_id`/`started_at`. Flattens
/// [`CommitOut`] so the shape stays lock-step with `/{session_id}/commits`.
#[cfg_attr(feature = "ts-export", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Serialize)]
pub struct CommitMapRowOut {
    pub kb: String,
    pub session_id: String,
    /// The session's NEWEST-capture artifact id (#11).
    pub artifact_id: String,
    pub started_at: i64,
    #[serde(flatten)]
    pub commit: CommitOut,
}

#[cfg_attr(feature = "ts-export", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Serialize)]
pub struct SessionsCommitMapResponse {
    pub commits: Vec<CommitMapRowOut>,
    pub limit: u32,
    pub offset: u32,
    /// `Some(offset + limit)` when the page came back full (there may be
    /// more); `None` on a short/empty page.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts-export", ts(optional))]
    pub next_offset: Option<u32>,
}

#[derive(Debug, Deserialize, Default)]
pub struct CommitMapParams {
    /// Floor on the owning session's `started_at` (unix seconds).
    pub since: Option<i64>,
    pub limit: Option<u32>,
    pub offset: Option<u32>,
}

/// `GET /api/sessions/commit-map?since=&limit=&offset=` (kb-code Wave 0 /
/// W0.6) — the flat bulk feed every `session_commits` row (newest-capture
/// only, #11), across every corpus, that kb-code's wave-3 sha→session join
/// precomputation consumes. Cross-kb fan-out (#28): each corpus's page is
/// over-fetched up to `offset + limit` rows (capped by
/// `COMMIT_MAP_MAX_PER_KB_FETCH`), merged newest-first with a deterministic
/// tiebreak, then sliced to the requested window — the same
/// overfetch-then-merge shape `sessions::list` uses for its keyset cursor,
/// adapted to a plain numeric offset.
pub async fn commit_map(
    State(state): State<Arc<KbHandles>>,
    Query(params): Query<CommitMapParams>,
) -> Response {
    let limit = params
        .limit
        .unwrap_or(COMMIT_MAP_DEFAULT_LIMIT)
        .clamp(1, COMMIT_MAP_MAX_LIMIT);
    let offset = params.offset.unwrap_or(0);
    let per_kb_fetch = offset
        .saturating_add(limit)
        .min(COMMIT_MAP_MAX_PER_KB_FETCH);
    let since = params.since;

    let mut futs: Vec<super::CorpusFut<'_, Vec<CommitMapRowOut>>> = Vec::new();
    for (kb_name, ctx) in state.kbs.iter() {
        futs.push(Box::pin(async move {
            let rows = match ctx
                .storage
                .session_commits_page(since, per_kb_fetch, 0)
                .await
            {
                Ok(r) => r,
                Err(e) => {
                    tracing::warn!(kb = %kb_name, error = %e, "session_commits_page failed");
                    return Vec::new();
                }
            };
            rows.into_iter()
                .map(|r| CommitMapRowOut {
                    kb: kb_name.as_str().to_string(),
                    session_id: r.commit.session_id.clone(),
                    artifact_id: r.commit.artifact_id_session.clone(),
                    started_at: r.started_at,
                    commit: CommitOut::from(r.commit),
                })
                .collect()
        }));
    }
    // PF-R1 — the operator-configurable `[server] fanout_cap` (default 8,
    // byte-identical to the old hardcoded `super::FANOUT_CAP`).
    let mut merged: Vec<CommitMapRowOut> = super::buffered_join(futs, state.fanout_cap)
        .await
        .into_iter()
        .flatten()
        .collect();
    merged.sort_by(|a, b| {
        b.started_at
            .cmp(&a.started_at)
            .then_with(|| a.session_id.cmp(&b.session_id))
            .then_with(|| a.kb.cmp(&b.kb))
    });
    let page: Vec<CommitMapRowOut> = merged
        .into_iter()
        .skip(offset as usize)
        .take(limit as usize)
        .collect();
    // A full page means there MIGHT be more (a hint, not a guarantee once
    // `per_kb_fetch` hit `COMMIT_MAP_MAX_PER_KB_FETCH`) — the caller pages
    // until a short/empty response, same contract as `offset`-paginated
    // `/api/kb/{kb}/docs` (`has_more`).
    let next_offset = (page.len() as u32 == limit).then(|| offset + limit);
    Json(SessionsCommitMapResponse {
        commits: page,
        limit,
        offset,
        next_offset,
    })
    .into_response()
}

/// R4 — one research / tool-usage signal a session produced.
#[cfg_attr(feature = "ts-export", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Serialize)]
pub struct ResearchOut {
    /// `kb_search | web | skill | subagent | plan_span | artifact_open`.
    pub kind: String,
    pub query: String,
}

#[cfg_attr(feature = "ts-export", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Serialize)]
pub struct SessionResearchResponse {
    pub research: Vec<ResearchOut>,
}

/// `GET /api/sessions/{session_id}/research` (R4) — the research / tool-usage
/// signals a session produced (kb & web searches, subagents, skills, plan
/// presentations), in order. "How kb was used / what was explored." Cheap
/// indexed read; detected, not ground truth (#10).
pub async fn research(
    State(state): State<Arc<KbHandles>>,
    Path(session_id): Path<String>,
) -> impl IntoResponse {
    let session_id = &session_id;
    let mut futs: Vec<super::CorpusFut<'_, Vec<kb_core::storage::sqlite::SessionResearchRow>>> =
        Vec::new();
    for (kb_name, ctx) in state.kbs.iter() {
        futs.push(Box::pin(async move {
            ctx.storage
                .session_research_for_session(session_id.clone())
                .await
                .unwrap_or_else(|e| {
                    tracing::warn!(kb = %kb_name, error = %e, "session_research_for_session failed");
                    Vec::new()
                })
        }));
    }
    // PF-R1 — the operator-configurable `[server] fanout_cap` (default 8,
    // byte-identical to the old hardcoded `super::FANOUT_CAP`).
    let mut rows: Vec<kb_core::storage::sqlite::SessionResearchRow> =
        super::buffered_join(futs, state.fanout_cap)
            .await
            .into_iter()
            .flatten()
            .collect();
    rows.sort_by_key(|r| r.seq);
    let research = rows
        .into_iter()
        .map(|r| ResearchOut {
            kind: r.kind,
            query: r.query,
        })
        .collect();
    Json(SessionResearchResponse { research })
}

// ---- R5 — session <-> comments link --------------------------------------

/// R5 — one open review comment on an artifact the session touched.
#[cfg_attr(feature = "ts-export", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Serialize)]
pub struct SessionCommentOut {
    pub comment_id: String,
    pub author: String,
    pub body: String,
    pub file_label: String,
}

/// R5 — an in-corpus artifact the session touched + the open comments on it.
#[cfg_attr(feature = "ts-export", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Serialize)]
pub struct SessionCommentArtifactOut {
    pub kb: String,
    pub artifact_id: String,
    pub title: String,
    /// Strongest action the session took on the artifact (edit > write > read).
    pub action: String,
    pub comments: Vec<SessionCommentOut>,
}

/// R8 — one comment RAISED during the session's time window (the temporal
/// dimension), possibly on an artifact the session never touched. `status` is
/// surfaced (a comment raised-then-resolved mid-session is still a signal),
/// never used to drop a row (#27). `touched` marks overlap with the structural
/// set. No "resolved during" — kb-comments/1 has no resolved_at (#6).
#[cfg_attr(feature = "ts-export", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Serialize)]
pub struct RaisedCommentOut {
    pub comment_id: String,
    pub kb: String,
    pub artifact_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts-export", ts(optional))]
    pub title: Option<String>,
    pub author: String,
    pub body: String,
    pub file_label: String,
    /// The comment's creation time (history.started_at).
    pub created_at: i64,
    /// `"open" | "resolved"` from the live review file.
    pub status: String,
    /// Also an artifact the session touched in-corpus.
    pub touched: bool,
}

#[cfg_attr(feature = "ts-export", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Serialize)]
pub struct SessionCommentsResponse {
    /// R5 — OPEN comments on artifacts the session TOUCHED (structural join).
    pub artifacts: Vec<SessionCommentArtifactOut>,
    pub total: usize,
    /// R8 — comments RAISED during the session's [started_at, ended_at] window
    /// (temporal join), newest-first, across all corpora.
    pub raised: Vec<RaisedCommentOut>,
}

/// `GET /api/sessions/{session_id}/comments` (R5) — the session<->comments
/// link (the one link that didn't exist). A computed-on-read JOIN, no edge
/// table (comments mutate independently of the session): the session's
/// in-corpus touched artifacts (`session_files.target_artifact_id`) → their
/// review files via the canonical `review::load` (never parsing `.review/*`
/// by hand — invariant #6). Returns OPEN comments only (the actionable set),
/// grouped per artifact. Federated per corpus (#28); bounded by the
/// touched-artifact count.
pub async fn session_comments(
    State(state): State<Arc<KbHandles>>,
    Path(session_id): Path<String>,
) -> impl IntoResponse {
    // R8 — resolve the session for its window (and 404 a bogus id, matching
    // /readings & /why).
    let Some(session) = find_session(&state, &session_id).await else {
        return error_to_problem_json(&kb_core::Error::NotFound(format!("session {session_id}")));
    };
    let raised_from = session.started_at;
    let raised_to = if session.ended_at > session.started_at {
        session.ended_at
    } else {
        session.started_at + RAISED_WINDOW_FALLBACK_SECS
    };

    // 1. The session's file edges (it may be captured in any corpus).
    let session_id = &session_id;
    let mut futs: Vec<super::CorpusFut<'_, Vec<kb_core::storage::sqlite::SessionFileRow>>> =
        Vec::new();
    for (kb_name, ctx) in state.kbs.iter() {
        futs.push(Box::pin(async move {
            ctx.storage
                .session_files_for_session(session_id.clone())
                .await
                .unwrap_or_else(|e| {
                    tracing::warn!(kb = %kb_name, error = %e, "session_files_for_session failed");
                    Vec::new()
                })
        }));
    }
    // PF-R1 — the operator-configurable `[server] fanout_cap` (default 8,
    // byte-identical to the old hardcoded `super::FANOUT_CAP`).
    let rows: Vec<kb_core::storage::sqlite::SessionFileRow> =
        super::buffered_join(futs, state.fanout_cap)
            .await
            .into_iter()
            .flatten()
            .collect();

    // 2. In-corpus targets, deduped by (kb, artifact_id), strongest action.
    let mut by_kb: std::collections::BTreeMap<String, std::collections::BTreeMap<String, u8>> =
        std::collections::BTreeMap::new();
    for r in &rows {
        if !r.in_corpus {
            continue;
        }
        let (Some(kb), Some(aid)) = (&r.target_kb, &r.target_artifact_id) else {
            continue;
        };
        let e = by_kb
            .entry(kb.clone())
            .or_default()
            .entry(aid.clone())
            .or_insert(0);
        *e = (*e).max(action_rank(&r.action));
    }
    // (No early-return on an empty touched set — a session can still have
    // comments RAISED during its window on artifacts it never touched.)

    // 3. Per corpus: load each target's review file (canonical loader, #6) and
    //    collect its OPEN comments.
    let by_kb = &by_kb;
    let mut futs2: Vec<super::CorpusFut<'_, Vec<SessionCommentArtifactOut>>> = Vec::new();
    for (kb_name, ctx) in state.kbs.iter() {
        let review_dir = state.paths.kb_review_dir(kb_name);
        futs2.push(Box::pin(async move {
            let Some(ids) = by_kb.get(kb_name.as_str()) else {
                return Vec::new();
            };
            let mut out = Vec::new();
            for (aid, rank) in ids {
                let path = review_dir.join(format!("{aid}.json"));
                let Ok(Some(file)) = kb_core::review::load(&path) else {
                    continue;
                };
                let comments: Vec<SessionCommentOut> = file
                    .comments
                    .iter()
                    .filter(|c| c.status == kb_core::review::CommentStatus::Open)
                    .map(|c| SessionCommentOut {
                        comment_id: c.id.clone(),
                        author: match c.author {
                            kb_core::review::Author::You => "you",
                            kb_core::review::Author::Claude => "claude",
                        }
                        .to_string(),
                        body: c.body.clone(),
                        file_label: c.file_label.clone(),
                    })
                    .collect();
                if comments.is_empty() {
                    continue;
                }
                let title = ctx
                    .storage
                    .get_by_id(aid.clone())
                    .await
                    .ok()
                    .flatten()
                    .map(|d| d.title)
                    .unwrap_or_else(|| file.artifact.title.clone());
                out.push(SessionCommentArtifactOut {
                    kb: kb_name.as_str().to_string(),
                    artifact_id: aid.clone(),
                    title,
                    action: rank_action(*rank).to_string(),
                    comments,
                });
            }
            out
        }));
    }
    // PF-R1 — the operator-configurable `[server] fanout_cap` (default 8,
    // byte-identical to the old hardcoded `super::FANOUT_CAP`).
    let mut artifacts: Vec<SessionCommentArtifactOut> =
        super::buffered_join(futs2, state.fanout_cap)
            .await
            .into_iter()
            .flatten()
            .collect();
    artifacts.sort_by(|a, b| {
        a.kb.cmp(&b.kb)
            .then_with(|| a.artifact_id.cmp(&b.artifact_id))
    });
    let total = artifacts.iter().map(|a| a.comments.len()).sum();

    // 4. R8 — comments RAISED during the session window, federated across each
    //    corpus's per-kb history table (#28). The touched set flags overlap.
    let touched: std::collections::HashSet<(String, String)> = by_kb
        .iter()
        .flat_map(|(kb, ids)| ids.keys().map(move |aid| (kb.clone(), aid.clone())))
        .collect();
    let touched_ref = &touched;
    let mut raised_futs: Vec<super::CorpusFut<'_, Vec<RaisedCommentOut>>> = Vec::new();
    for (kb_name, ctx) in state.kbs.iter() {
        let review_dir = state.paths.kb_review_dir(kb_name);
        raised_futs.push(Box::pin(async move {
            let rows = ctx
                .storage
                .history_comments_in_window(raised_from, raised_to, RAISED_SCAN_CAP)
                .await
                .unwrap_or_else(|e| {
                    tracing::warn!(kb = %kb_name, error = %e, "history_comments_in_window failed");
                    Vec::new()
                });
            let mut out = Vec::new();
            // Memoise per artifact within this corpus's window: the review file
            // (a disk read + JSON parse) and its resolved title (a lance read)
            // are otherwise re-fetched for every raised comment sharing an
            // artifact. Read-only display cache, local to this future — never a
            // mutation (invariant #6 doesn't apply).
            let mut review_cache: std::collections::HashMap<
                String,
                Option<kb_core::review::ReviewFile>,
            > = std::collections::HashMap::new();
            let mut title_cache: std::collections::HashMap<String, String> =
                std::collections::HashMap::new();
            for r in rows {
                let (Some(aid), Some(cid)) = (r.artifact_id, r.comment_id) else {
                    continue;
                };
                if !review_cache.contains_key(&aid) {
                    let loaded = kb_core::review::load(&review_dir.join(format!("{aid}.json")))
                        .ok()
                        .flatten();
                    review_cache.insert(aid.clone(), loaded);
                }
                let Some(file) = review_cache.get(&aid).and_then(|f| f.as_ref()) else {
                    continue;
                };
                let Some(c) = file.comments.iter().find(|c| c.id == cid) else {
                    continue;
                };
                let title = Some(if let Some(t) = title_cache.get(&aid) {
                    t.clone()
                } else {
                    let t = match ctx.storage.get_by_id(aid.clone()).await {
                        Ok(Some(d)) => d.title,
                        _ => file.artifact.title.clone(),
                    };
                    title_cache.insert(aid.clone(), t.clone());
                    t
                });
                out.push(RaisedCommentOut {
                    comment_id: cid,
                    kb: kb_name.as_str().to_string(),
                    artifact_id: aid.clone(),
                    title,
                    author: match c.author {
                        kb_core::review::Author::You => "you",
                        kb_core::review::Author::Claude => "claude",
                    }
                    .to_string(),
                    body: c.body.clone(),
                    file_label: c.file_label.clone(),
                    created_at: r.started_at_unix,
                    status: match c.status {
                        kb_core::review::CommentStatus::Open => "open",
                        kb_core::review::CommentStatus::Resolved => "resolved",
                    }
                    .to_string(),
                    touched: touched_ref.contains(&(kb_name.as_str().to_string(), aid)),
                });
            }
            out
        }));
    }
    // PF-R1 — the operator-configurable `[server] fanout_cap` (default 8,
    // byte-identical to the old hardcoded `super::FANOUT_CAP`).
    let mut raised: Vec<RaisedCommentOut> = super::buffered_join(raised_futs, state.fanout_cap)
        .await
        .into_iter()
        .flatten()
        .collect();
    raised.sort_by(|a, b| {
        b.created_at
            .cmp(&a.created_at)
            .then_with(|| a.kb.cmp(&b.kb))
            .then_with(|| a.comment_id.cmp(&b.comment_id))
    });

    Json(SessionCommentsResponse {
        artifacts,
        total,
        raised,
    })
    .into_response()
}

/// One session that touched a given artifact (A7, the reverse direction).
#[cfg_attr(feature = "ts-export", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Serialize)]
pub struct ArtifactSessionOut {
    pub session_id: String,
    pub kb: String,
    pub started_at: i64,
    pub display_name: String,
    /// Strongest action this session took on the artifact (edit > write > read);
    /// `authored`-only sessions (no file row) report `write` (creation). Kept for
    /// sort/back-compat — the SPA filters on the per-action booleans below.
    pub action: String,
    /// AS — the DISTINCT actions this session took on THIS artifact. A session
    /// that read then edited reports `read=true, edited=true` (not collapsed to a
    /// single strongest action), so the read/write filter is correct.
    pub read: bool,
    pub wrote: bool,
    pub edited: bool,
    /// AS — this session is the artifact's origin: `doc.kb_session == session_id`
    /// (the `<meta name="kb-session">` the artifact was born in). Unioned in even
    /// when the origin left no file row (e.g. authored via `kb remember`).
    pub authored: bool,
    /// AS — the session's opening prompt (cheap, from the persisted row).
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts-export", ts(optional))]
    pub first_user_prompt: Option<String>,
    /// AS — steering decisions + git actions, populated only for the top
    /// mutating/authoring sessions (`REASONING_MAX`); empty for read-only rows.
    pub decisions: Vec<DecisionOut>,
    pub commits: Vec<CommitOut>,
}

/// AS — the distinct tool actions one session took on one artifact, folded from
/// its `session_files` rows. `strongest`/back-compat `action` is derived; the
/// booleans drive the SPA's read/write filter.
#[derive(Default)]
struct ArtifactActionSet {
    read: bool,
    wrote: bool,
    edited: bool,
}

/// AS — per-artifact reasoning enrichment cap (mirrors `WHY_MAX_SESSIONS`): a hot
/// artifact can't amplify into an unbounded per-session decisions/commits fan-out.
const REASONING_MAX: usize = 8;

#[cfg_attr(feature = "ts-export", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Serialize)]
pub struct ArtifactSessionsResponse {
    pub sessions: Vec<ArtifactSessionOut>,
}

/// `GET /api/artifacts/{kb}/{artifact_id}/sessions` — the sessions that
/// worked with this artifact (A7/AS), newest-first. Backed by the indexed
/// `session_files.target_artifact_id`; fans out across corpora because a
/// session captured in one kb edits files in another. Each session reports the
/// DISTINCT actions it took (read/wrote/edited — not collapsed to one), an
/// `authored` flag for the artifact's origin session (lance `kb_session`), and —
/// for the top mutating/authoring sessions — its steering decisions + commits.
pub async fn artifact_sessions(
    State(state): State<Arc<KbHandles>>,
    Path((kb, artifact_id)): Path<(String, String)>,
) -> impl IntoResponse {
    let artifact_id = &artifact_id;
    let mut futs: Vec<super::CorpusFut<'_, Vec<kb_core::storage::sqlite::SessionFileRow>>> =
        Vec::new();
    for (kb_name, ctx) in state.kbs.iter() {
        futs.push(Box::pin(async move {
            ctx.storage
                .session_files_for_artifact(artifact_id.clone())
                .await
                .unwrap_or_else(|e| {
                    tracing::warn!(kb = %kb_name, error = %e, "session_files_for_artifact failed");
                    Vec::new()
                })
        }));
    }
    // PF-R1 — the operator-configurable `[server] fanout_cap` (default 8,
    // byte-identical to the old hardcoded `super::FANOUT_CAP`).
    let rows: Vec<kb_core::storage::sqlite::SessionFileRow> =
        super::buffered_join(futs, state.fanout_cap)
            .await
            .into_iter()
            .flatten()
            .collect();

    // Fold per session into its distinct action set (read+edit stays read+edit).
    let mut sets: std::collections::BTreeMap<String, ArtifactActionSet> =
        std::collections::BTreeMap::new();
    for r in &rows {
        let e = sets.entry(r.session_id.clone()).or_default();
        match r.action.as_str() {
            "read" => e.read = true,
            "write" => e.wrote = true,
            "edit" => e.edited = true,
            _ => {}
        }
    }

    // Origin: the session this artifact was born in (`<meta kb-session>` → lance
    // `kb_session`, resolved against the path kb). Union it in even when it left
    // no file row, so "which session created this" is never silently missing.
    let origin: Option<String> = match super::resolve_kb(&state, &kb) {
        Ok((_, ctx)) => ctx
            .storage
            .get_by_id(artifact_id.clone())
            .await
            .ok()
            .flatten()
            .and_then(|d| d.kb_session),
        Err(_) => None,
    };
    if let Some(sid) = &origin {
        sets.entry(sid.clone()).or_default();
    }

    // Resolve every touching session's row (display_name/started_at/kb/prompt)
    // in ONE batched metadata fan-out — `sessions_get_many` per corpus, first-
    // wins in submission (BTreeMap) order — instead of a per-session
    // `lookup_session` fan-out awaited serially. Same result (sessions_get_many
    // returns the newest capture per id, #11); memory_count isn't surfaced here
    // so this skips the second per-session count fan-out too.
    let ids: Vec<String> = sets.keys().cloned().collect();
    let ids_ref = &ids;
    let mut meta_futs: Vec<super::CorpusFut<'_, Vec<(String, SessionOut)>>> = Vec::new();
    for (kb_name, ctx) in state.kbs.iter() {
        meta_futs.push(Box::pin(async move {
            ctx.storage
                .sessions_get_many(ids_ref.clone())
                .await
                .map(|rows| {
                    rows.into_iter()
                        .map(|row| {
                            let so = SessionOut::from_row(kb_name.as_str(), row);
                            (so.session_id.clone(), so)
                        })
                        .collect::<Vec<_>>()
                })
                .unwrap_or_else(|e| {
                    tracing::warn!(kb = %kb_name, error = %e, "sessions_get_many failed");
                    Vec::new()
                })
        }));
    }
    let mut meta: std::collections::BTreeMap<String, SessionOut> =
        std::collections::BTreeMap::new();
    // PF-R1 — the operator-configurable `[server] fanout_cap` (default 8,
    // byte-identical to the old hardcoded `super::FANOUT_CAP`).
    for (sid, so) in super::buffered_join(meta_futs, state.fanout_cap)
        .await
        .into_iter()
        .flatten()
    {
        meta.entry(sid).or_insert(so); // first-wins in submission order
    }

    let mut out = Vec::with_capacity(sets.len());
    for (sid, set) in sets {
        let authored = origin.as_deref() == Some(sid.as_str());
        // Back-compat strongest: a creation-only origin reports `write`.
        let action = if set.edited {
            "edit"
        } else if set.wrote || authored {
            "write"
        } else {
            "read"
        };
        if let Some(so) = meta.remove(&sid) {
            out.push(ArtifactSessionOut {
                session_id: so.session_id,
                kb: so.kb,
                started_at: so.started_at,
                display_name: so.display_name,
                action: action.to_string(),
                read: set.read,
                wrote: set.wrote,
                edited: set.edited,
                authored,
                first_user_prompt: so.first_user_prompt,
                decisions: Vec::new(),
                commits: Vec::new(),
            });
        }
    }
    out.sort_by(|a, b| {
        b.started_at
            .cmp(&a.started_at)
            .then_with(|| a.session_id.cmp(&b.session_id))
    });

    // Reasoning, capped: enrich the top mutating/authoring sessions with their
    // steering decisions + git actions. Read-only sessions just consulted the
    // file — they carry no "why", so they're skipped (and don't burn the cap).
    // Pick the enriched rows first (same order/cap as the old serial loop), then
    // drive their decisions+commits fetches concurrently (submission-ordered,
    // #28) rather than two serial cross-kb fan-outs per session.
    let enrich_idxs: Vec<usize> = out
        .iter()
        .enumerate()
        .filter(|(_, s)| s.wrote || s.edited || s.authored)
        .map(|(i, _)| i)
        .take(REASONING_MAX)
        .collect();
    let state_ref = &state;
    let mut enrich_futs: Vec<super::CorpusFut<'_, (Vec<DecisionOut>, Vec<CommitOut>)>> =
        Vec::with_capacity(enrich_idxs.len());
    for &i in &enrich_idxs {
        let sid = out[i].session_id.clone();
        enrich_futs.push(Box::pin(async move {
            tokio::join!(
                fetch_decisions(state_ref, &sid),
                fetch_commits(state_ref, &sid)
            )
        }));
    }
    // PF-R1 — the operator-configurable `[server] fanout_cap` (default 8,
    // byte-identical to the old hardcoded `super::FANOUT_CAP`).
    let enriched = super::buffered_join(enrich_futs, state.fanout_cap).await;
    for (&i, (decisions, commits)) in enrich_idxs.iter().zip(enriched) {
        out[i].decisions = decisions;
        out[i].commits = commits;
    }

    Json(ArtifactSessionsResponse { sessions: out })
}

/// Action severity for collapsing/sorting: edit > write > read.
fn action_rank(action: &str) -> u8 {
    match action {
        "edit" => 3,
        "write" => 2,
        "read" => 1,
        _ => 0,
    }
}

fn rank_action(rank: u8) -> &'static str {
    match rank {
        3 => "edit",
        2 => "write",
        _ => "read",
    }
}

/// R2 — query params for `GET /api/why`.
#[derive(Debug, Deserialize)]
pub struct WhyParams {
    /// The file to explain (absolute or source-relative; the basename is the
    /// robust join key, refined to exact by path alignment).
    pub path: String,
}

/// R2 — one session that touched the queried file, with the reasoning the
/// transcript recorded. Ordered exact-first, then strongest action, then most
/// recent.
#[cfg_attr(feature = "ts-export", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Serialize)]
pub struct WhySessionOut {
    pub session_id: String,
    pub kb: String,
    pub display_name: String,
    pub started_at: i64,
    /// Strongest action this session took on the file (edit > write > read).
    pub action: String,
    /// `"exact"` (the stored path aligns with the queried one) or `"fuzzy"`
    /// (only the basename matched — possibly a same-named file elsewhere).
    pub confidence: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts-export", ts(optional))]
    pub first_user_prompt: Option<String>,
    /// V0029/R3 — the closure (`SessionOut::outcome` verbatim), for the
    /// P10 `project:`/`closed:` header lines `kb why` prints per session.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts-export", ts(optional))]
    pub outcome: Option<String>,
    pub harness: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts-export", ts(optional))]
    pub project_key: Option<String>,
    pub decisions: Vec<DecisionOut>,
    pub commits: Vec<CommitOut>,
}

#[cfg_attr(feature = "ts-export", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Serialize)]
pub struct WhyResponse {
    pub path: String,
    pub basename: String,
    pub sessions: Vec<WhySessionOut>,
}

/// R2 cap — the WHY assembler details at most this many touching sessions
/// (strongest + most-recent first), so a hot file can't trigger an unbounded
/// per-session decisions/commits fan-out.
const WHY_MAX_SESSIONS: usize = 8;

/// R8 — when a session's `ended_at` is missing/degenerate (a crashed session
/// whose transcript carried no event timestamps, so ended_at == started_at),
/// cap the "raised during" window forward from `started_at` by this much.
const RAISED_WINDOW_FALLBACK_SECS: i64 = 6 * 60 * 60;
/// R8 — per-corpus scan cap for the comment-creation history window.
const RAISED_SCAN_CAP: u32 = 500;

/// True when two file paths refer to the same file despite differing prefixes
/// (absolute vs source-relative): equal, or one is a path-COMPONENT suffix of
/// the other. Promotes a basename match to "exact". Component-aligned so
/// `parser.rs` is not treated as a suffix of `myparser.rs`.
fn paths_align(a: &str, b: &str) -> bool {
    let norm = |s: &str| {
        s.replace('\\', "/")
            .trim_start_matches("./")
            .trim_start_matches('/')
            .to_string()
    };
    let (a, b) = (norm(a), norm(b));
    if a == b {
        return true;
    }
    let suffix_aligned = |long: &str, short: &str| {
        long.len() > short.len()
            && long.ends_with(short)
            && long.as_bytes()[long.len() - short.len() - 1] == b'/'
    };
    suffix_aligned(&a, &b) || suffix_aligned(&b, &a)
}

/// R2 — fan out + order one session's decisions (reused by `why`).
async fn fetch_decisions(state: &Arc<KbHandles>, session_id: &str) -> Vec<DecisionOut> {
    let session_id = &session_id;
    let mut futs: Vec<super::CorpusFut<'_, Vec<kb_core::storage::sqlite::SessionDecisionRow>>> =
        Vec::new();
    for (kb_name, ctx) in state.kbs.iter() {
        futs.push(Box::pin(async move {
            ctx.storage
                .session_decisions_for_session(session_id.to_string())
                .await
                .unwrap_or_else(|e| {
                    tracing::warn!(kb = %kb_name, error = %e, "session_decisions_for_session failed");
                    Vec::new()
                })
        }));
    }
    // PF-R1 — the operator-configurable `[server] fanout_cap` (default 8,
    // byte-identical to the old hardcoded `super::FANOUT_CAP`).
    let mut rows: Vec<kb_core::storage::sqlite::SessionDecisionRow> =
        super::buffered_join(futs, state.fanout_cap)
            .await
            .into_iter()
            .flatten()
            .collect();
    rows.sort_by_key(|r| r.seq);
    rows.into_iter()
        .map(|r| DecisionOut {
            kind: r.kind,
            prompt: r.prompt,
            answer: r.answer,
        })
        .collect()
}

/// R2 — fan out + order one session's git actions (reused by `why`).
async fn fetch_commits(state: &Arc<KbHandles>, session_id: &str) -> Vec<CommitOut> {
    let session_id = &session_id;
    let mut futs: Vec<super::CorpusFut<'_, Vec<kb_core::storage::sqlite::SessionCommitRow>>> =
        Vec::new();
    for (kb_name, ctx) in state.kbs.iter() {
        futs.push(Box::pin(async move {
            ctx.storage
                .session_commits_for_session(session_id.to_string())
                .await
                .unwrap_or_else(|e| {
                    tracing::warn!(kb = %kb_name, error = %e, "session_commits_for_session failed");
                    Vec::new()
                })
        }));
    }
    // PF-R1 — the operator-configurable `[server] fanout_cap` (default 8,
    // byte-identical to the old hardcoded `super::FANOUT_CAP`).
    let mut rows: Vec<kb_core::storage::sqlite::SessionCommitRow> =
        super::buffered_join(futs, state.fanout_cap)
            .await
            .into_iter()
            .flatten()
            .collect();
    rows.sort_by_key(|r| r.seq);
    rows.into_iter().map(CommitOut::from).collect()
}

/// `GET /api/why?path=<file>` (R2) — the WHY assembler. Finds the sessions
/// that touched a file (by basename, refined to exact/fuzzy by path
/// alignment), ordered exact-first then by action strength and recency, and
/// inlines each session's opening prompt + steering decisions + commits: a
/// deterministic "this file is like this because you asked X, decided Y, it
/// shipped as Z." Zero new index — pure joins over the session_* tables;
/// fans out per corpus (#28) and batches the session-metadata lookup so a hot
/// file can't amplify into a per-session fan-out.
pub async fn why(
    State(state): State<Arc<KbHandles>>,
    Query(params): Query<WhyParams>,
) -> impl IntoResponse {
    let path = params.path.trim().to_string();
    if path.is_empty() {
        return error_to_problem_json(&kb_core::Error::BadRequest("path is required".into()));
    }
    let basename = path
        .replace('\\', "/")
        .rsplit('/')
        .next()
        .unwrap_or(&path)
        .to_string();
    if basename.is_empty() {
        return error_to_problem_json(&kb_core::Error::BadRequest("path has no file name".into()));
    }

    // 1. Fan out the basename match across corpora (a session captured in one
    //    kb edits files anywhere).
    let bn = &basename;
    let mut futs: Vec<super::CorpusFut<'_, Vec<kb_core::storage::sqlite::SessionFileRow>>> =
        Vec::new();
    for (kb_name, ctx) in state.kbs.iter() {
        futs.push(Box::pin(async move {
            ctx.storage
                .session_files_for_basename(bn.clone())
                .await
                .unwrap_or_else(|e| {
                    tracing::warn!(kb = %kb_name, error = %e, "session_files_for_basename failed");
                    Vec::new()
                })
        }));
    }
    // PF-R1 — the operator-configurable `[server] fanout_cap` (default 8,
    // byte-identical to the old hardcoded `super::FANOUT_CAP`).
    let rows: Vec<kb_core::storage::sqlite::SessionFileRow> =
        super::buffered_join(futs, state.fanout_cap)
            .await
            .into_iter()
            .flatten()
            .collect();

    // 2. Collapse per session: strongest action + best confidence.
    struct Agg {
        action_rank: u8,
        exact: bool,
    }
    let mut by_session: std::collections::BTreeMap<String, Agg> = std::collections::BTreeMap::new();
    for r in &rows {
        let exact = paths_align(&r.path, &path);
        let e = by_session.entry(r.session_id.clone()).or_insert(Agg {
            action_rank: 0,
            exact: false,
        });
        e.action_rank = e.action_rank.max(action_rank(&r.action));
        e.exact = e.exact || exact;
    }
    if by_session.is_empty() {
        return Json(WhyResponse {
            path,
            basename,
            sessions: Vec::new(),
        })
        .into_response();
    }

    // 3. Batched session metadata (one IN-query per corpus; first-wins).
    let ids: Vec<String> = by_session.keys().cloned().collect();
    let ids_ref = &ids;
    let mut meta_futs: Vec<super::CorpusFut<'_, Vec<(String, SessionOut)>>> = Vec::new();
    for (kb_name, ctx) in state.kbs.iter() {
        meta_futs.push(Box::pin(async move {
            ctx.storage
                .sessions_get_many(ids_ref.clone())
                .await
                .map(|rows| {
                    rows.into_iter()
                        .map(|row| {
                            let so = SessionOut::from_row(kb_name.as_str(), row);
                            (so.session_id.clone(), so)
                        })
                        .collect::<Vec<_>>()
                })
                .unwrap_or_else(|e| {
                    tracing::warn!(kb = %kb_name, error = %e, "sessions_get_many failed");
                    Vec::new()
                })
        }));
    }
    let mut meta: std::collections::BTreeMap<String, SessionOut> =
        std::collections::BTreeMap::new();
    // PF-R1 — the operator-configurable `[server] fanout_cap` (default 8,
    // byte-identical to the old hardcoded `super::FANOUT_CAP`).
    for (sid, so) in super::buffered_join(meta_futs, state.fanout_cap)
        .await
        .into_iter()
        .flatten()
    {
        meta.entry(sid).or_insert(so); // first-wins in submission order
    }

    // 4. Build + sort candidates (exact-first, action desc, recency desc), cap.
    let mut candidates: Vec<(String, Agg, SessionOut)> = Vec::new();
    for (sid, agg) in by_session {
        if let Some(so) = meta.remove(&sid) {
            candidates.push((sid, agg, so));
        }
    }
    candidates.sort_by(|a, b| {
        b.1.exact
            .cmp(&a.1.exact)
            .then_with(|| b.1.action_rank.cmp(&a.1.action_rank))
            .then_with(|| b.2.started_at.cmp(&a.2.started_at))
            .then_with(|| a.0.cmp(&b.0))
    });
    candidates.truncate(WHY_MAX_SESSIONS);

    // 5. Per capped session: decisions + commits (bounded fan-outs). Drive
    // every session's pair concurrently (submission-ordered, #28) rather than
    // two serial cross-kb fan-outs each; results zip back in candidate order.
    let state_ref = &state;
    let mut enrich_futs: Vec<super::CorpusFut<'_, (Vec<DecisionOut>, Vec<CommitOut>)>> =
        Vec::with_capacity(candidates.len());
    for (sid, _agg, _so) in &candidates {
        let sid = sid.clone();
        enrich_futs.push(Box::pin(async move {
            tokio::join!(
                fetch_decisions(state_ref, &sid),
                fetch_commits(state_ref, &sid)
            )
        }));
    }
    // PF-R1 — the operator-configurable `[server] fanout_cap` (default 8,
    // byte-identical to the old hardcoded `super::FANOUT_CAP`).
    let enriched = super::buffered_join(enrich_futs, state.fanout_cap).await;
    let mut sessions = Vec::with_capacity(candidates.len());
    for ((_sid, agg, so), (decisions, commits)) in candidates.into_iter().zip(enriched) {
        sessions.push(WhySessionOut {
            session_id: so.session_id,
            kb: so.kb,
            display_name: so.display_name,
            started_at: so.started_at,
            action: rank_action(agg.action_rank).to_string(),
            confidence: if agg.exact { "exact" } else { "fuzzy" }.to_string(),
            first_user_prompt: so.first_user_prompt,
            outcome: so.outcome,
            harness: so.harness,
            project_key: so.project_key,
            decisions,
            commits,
        });
    }

    Json(WhyResponse {
        path,
        basename,
        sessions,
    })
    .into_response()
}

// ---- R3 — `kb recollect` ---------------------------------------------------

const RECOLLECT_DEFAULT_LIMIT: u32 = 8;
const RECOLLECT_MAX_LIMIT: u32 = 30;
/// Per-corpus candidate pool — over-fetch so a session ranked just past the
/// page still reaches the cross-kb rerank.
const RECOLLECT_POOL: u32 = 60;

#[derive(Debug, Deserialize)]
pub struct RecollectParams {
    /// Free-text query. Required UNLESS `similar_to` is set (exactly one).
    pub q: Option<String>,
    /// R7 — find sessions similar to THIS session: its indexed digest excerpt
    /// becomes the query and the source session is excluded from the results.
    /// Mutually exclusive with `q`.
    pub similar_to: Option<String>,
    /// Restrict to one project (the `folder` basename or full cwd).
    pub folder: Option<String>,
    /// W3.A — restrict to one project (registry id or raw `project_key`).
    /// Composes (AND) with `folder=` when both are given; applied Rust-side
    /// alongside the existing `folder`/`since` post-fetch filters (step 4
    /// below — recollect already does a full metadata fetch before
    /// filtering, so there's no cursor to desync).
    pub project: Option<String>,
    /// Relative recency window over `started_at`: `day|week|month|year`.
    pub since: Option<String>,
    pub limit: Option<u32>,
}

/// R3 — one recollected session: the match + the SURFACED signals the agent
/// weighs (recency/staleness, errors, commits) and the reasoning preview.
#[cfg_attr(feature = "ts-export", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Serialize)]
pub struct RecollectSessionOut {
    pub session_id: String,
    pub kb: String,
    pub display_name: String,
    pub started_at: i64,
    pub score: f32,
    /// Whole-day age — a SURFACED staleness signal, never used to drop a hit.
    pub age_days: i64,
    /// `true` past the staleness threshold: the rationale may predate later
    /// rewrites of the same code. Surfaced, never filtered (#27).
    pub stale: bool,
    /// Coarse "did things go wrong" signal (detected error tool-results).
    pub error_count: u32,
    /// Git actions the session shipped (detected, not ground truth).
    pub commit_count: u32,
    pub committed: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts-export", ts(optional))]
    pub first_user_prompt: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts-export", ts(optional))]
    pub folder: Option<String>,
    /// The digest excerpt — what actually matched.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts-export", ts(optional))]
    pub summary: Option<String>,
    /// V0029/R3 — the closure (`SessionOut::outcome` verbatim), for the
    /// P10 `closed:` line `kb recollect` prints per hit.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts-export", ts(optional))]
    pub outcome: Option<String>,
    pub harness: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts-export", ts(optional))]
    pub project_key: Option<String>,
}

#[cfg_attr(feature = "ts-export", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Serialize)]
pub struct RecollectResponse {
    pub sessions: Vec<RecollectSessionOut>,
    pub ms: u64,
}

fn since_window_secs(s: &str) -> Option<i64> {
    match s.trim() {
        "day" => Some(86_400),
        "week" => Some(7 * 86_400),
        "month" => Some(30 * 86_400),
        "year" => Some(365 * 86_400),
        _ => None,
    }
}

/// `GET /api/sessions/recollect?q=&folder=&since=&limit=` (R3) — semantic
/// "has something like this been done?" over the R1 insight digests. Federated
/// hybrid (or BM25) search restricted to session digests, re-ranked
/// deterministically by relevance × recency with the success / error / recency
/// signals SURFACED (never buried) and a query-time staleness flag. Pull-only:
/// the agent invokes it deliberately; nothing is auto-injected.
pub async fn recollect(
    State(state): State<Arc<KbHandles>>,
    Query(params): Query<RecollectParams>,
) -> impl IntoResponse {
    match recollect_compose(state, params).await {
        Ok(out) => Json(out).into_response(),
        Err(resp) => resp,
    }
}

/// The recollect engine, split out of the axum handler so a SECOND
/// in-process caller can compose it without an HTTP round-trip: CT-D1's
/// `GET /api/context` pack (`routes::context`) uses it for the session
/// POINTERS lane. R0/R1/R3 are untouched by that reuse — this function
/// returns exactly what it always returned (digest excerpts + surfaced
/// signals, never transcript bodies), and the pack narrows it further
/// rather than widening it.
///
/// Takes its arguments OWNED, exactly as the extractors delivered them, so
/// the body below is unchanged from the handler it was lifted out of.
#[allow(clippy::result_large_err)]
pub(crate) async fn recollect_compose(
    state: Arc<KbHandles>,
    params: RecollectParams,
) -> Result<RecollectResponse, Response> {
    let started = std::time::Instant::now();
    // R7 — exactly one of `q` / `similar_to`. With `similar_to`, the source
    // session's indexed digest excerpt becomes the query (digest-vs-digest),
    // and the source is excluded from the results.
    let q_param = params.q.as_deref().map(str::trim).filter(|s| !s.is_empty());
    let similar_to = params
        .similar_to
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty());
    let (q, exclude_session) = match (q_param, similar_to) {
        (Some(_), Some(_)) => {
            return Err(error_to_problem_json(&kb_core::Error::BadRequest(
                "pass exactly one of q / similar_to, not both".into(),
            )));
        }
        (Some(q), None) => (q.to_string(), None),
        (None, Some(sid)) => match resolve_session_digest_query(&state, sid).await {
            Some(text) if !text.trim().is_empty() => (text, Some(sid.to_string())),
            _ => {
                return Err(error_to_problem_json(&kb_core::Error::NotFound(format!(
                    "session {sid} has no indexed digest to compare against"
                ))));
            }
        },
        (None, None) => {
            return Err(error_to_problem_json(&kb_core::Error::BadRequest(
                "q or similar_to is required".into(),
            )));
        }
    };
    let limit = params
        .limit
        .unwrap_or(RECOLLECT_DEFAULT_LIMIT)
        .clamp(1, RECOLLECT_MAX_LIMIT) as usize;

    // Embed the query once per distinct embedder model (mirrors recall).
    let mut vec_by_model: std::collections::HashMap<&'static str, Vec<f32>> =
        std::collections::HashMap::new();
    for (_, ctx) in state.kbs.iter() {
        if let Some(emb) = &ctx.embedder {
            let model = emb.lock().unwrap_or_else(|e| e.into_inner()).model_name();
            if let std::collections::hash_map::Entry::Vacant(slot) = vec_by_model.entry(model) {
                if let Ok(out) = crate::embed_cache::embed_query(&state.embed_cache, emb, &q).await
                {
                    slot.insert(out.vec);
                }
            }
        }
    }

    // 1. Per-corpus search → keep ONLY session digests, dense-rank them.
    struct Hit {
        /// #11 — always the CANONICAL (JSONL-recovered) sqlite id, resolved
        /// per-corpus below via `sessions_get_by_artifact_ids`; never the raw
        /// lance `kb_session` meta.
        session_id: String,
        kb: String,
        summary: Option<String>,
        rank: usize,
    }
    let q_ref = &q;
    let vbm = &vec_by_model;
    let mut futs: Vec<super::CorpusFut<'_, Vec<Hit>>> = Vec::new();
    for (kb_name, ctx) in state.kbs.iter() {
        futs.push(Box::pin(async move {
            // ensure_* returns Ok when the index already exists; a real Err
            // would make the query fail with a less-specific message. Skip
            // this corpus (invariant #28) rather than swallow-and-continue.
            if let Err(e) = ctx.storage.ensure_fts_index().await {
                tracing::warn!(
                    kb = %kb_name,
                    error = %e,
                    "recollect: ensure_fts_index failed; skipping corpus"
                );
                return Vec::new();
            }
            // Drop the embedder guard before any await (#15): model_name()
            // returns &'static str.
            let model_vec = ctx.embedder.as_ref().and_then(|emb| {
                let m = emb.lock().unwrap_or_else(|e| e.into_inner()).model_name();
                vbm.get(m).cloned()
            });
            let rows = match model_vec {
                Some(v) => {
                    ctx.storage
                        .hybrid_query(q_ref.clone(), v, RECOLLECT_POOL)
                        .await
                }
                None => {
                    ctx.storage
                        .bm25_query(q_ref.clone(), RECOLLECT_POOL, false)
                        .await
                }
            };
            let rows = rows.unwrap_or_else(|e| {
                tracing::warn!(kb = %kb_name, error = %e, "recollect query failed");
                Vec::new()
            });
            // Keep only session digests; dense-rank them (rank 0 = best match
            // among this corpus's sessions). Identity is carried as the lance
            // artifact id — NOT `d.kb_session`, which is the raw
            // `<meta name="kb-session">` value and can be a capture-hook-dirty
            // hint (trailing dash / truncation, #11); the canonical session id
            // is resolved below via the sqlite `artifact_id` join.
            let cands: Vec<(usize, String, Option<String>)> = rows
                .into_iter()
                .filter(|d| {
                    d.kb_category.as_deref() == Some(kb_core::sessions::MEMORY_SESSION_CATEGORY)
                })
                .enumerate()
                .map(|(rank, d)| (rank, d.id, d.summary))
                .collect();
            if cands.is_empty() {
                return Vec::new();
            }
            let artifact_ids: Vec<String> = cands.iter().map(|(_, id, _)| id.clone()).collect();
            let session_rows = ctx
                .storage
                .sessions_get_by_artifact_ids(artifact_ids)
                .await
                .unwrap_or_else(|e| {
                    tracing::warn!(kb = %kb_name, error = %e, "sessions_get_by_artifact_ids failed");
                    Vec::new()
                });
            let by_artifact: std::collections::HashMap<String, String> = session_rows
                .into_iter()
                .map(|r| (r.artifact_id, r.session_id))
                .collect();
            cands
                .into_iter()
                .filter_map(|(rank, artifact_id, summary)| {
                    let session_id = by_artifact.get(&artifact_id)?.clone();
                    Some(Hit {
                        session_id,
                        kb: kb_name.as_str().to_string(),
                        summary,
                        rank,
                    })
                })
                .collect::<Vec<_>>()
        }));
    }
    // PF-R1 — the operator-configurable `[server] fanout_cap` (default 8,
    // byte-identical to the old hardcoded `super::FANOUT_CAP`).
    let raw_hits: Vec<Hit> = super::buffered_join(futs, state.fanout_cap)
        .await
        .into_iter()
        .flatten()
        .collect();
    if raw_hits.is_empty() {
        return Ok(RecollectResponse {
            sessions: Vec::new(),
            ms: started.elapsed().as_millis() as u64,
        });
    }

    // 2. Dedup by session id (best/lowest rank; first-wins on ties).
    // R7 — never return the source session as "similar to itself"; excluded at
    // dedup (before rerank/truncate) so it can't consume a result slot.
    let exclude = exclude_session.as_deref();
    let mut best: std::collections::BTreeMap<String, Hit> = std::collections::BTreeMap::new();
    for h in raw_hits {
        if Some(h.session_id.as_str()) == exclude {
            continue;
        }
        match best.get(&h.session_id) {
            Some(prev) if prev.rank <= h.rank => {}
            _ => {
                best.insert(h.session_id.clone(), h);
            }
        }
    }

    // 3. Batch session metadata (one IN-query per corpus; first-wins).
    let ids: Vec<String> = best.keys().cloned().collect();
    let ids_ref = &ids;
    let mut meta_futs: Vec<super::CorpusFut<'_, Vec<(String, SessionOut)>>> = Vec::new();
    for (kb_name, ctx) in state.kbs.iter() {
        meta_futs.push(Box::pin(async move {
            ctx.storage
                .sessions_get_many(ids_ref.clone())
                .await
                .map(|rows| {
                    rows.into_iter()
                        .map(|row| {
                            let so = SessionOut::from_row(kb_name.as_str(), row);
                            (so.session_id.clone(), so)
                        })
                        .collect::<Vec<_>>()
                })
                .unwrap_or_default()
        }));
    }
    let mut meta: std::collections::BTreeMap<String, SessionOut> =
        std::collections::BTreeMap::new();
    // PF-R1 — the operator-configurable `[server] fanout_cap` (default 8,
    // byte-identical to the old hardcoded `super::FANOUT_CAP`).
    for (sid, so) in super::buffered_join(meta_futs, state.fanout_cap)
        .await
        .into_iter()
        .flatten()
    {
        meta.entry(sid).or_insert(so);
    }

    // 4. since/folder filters; build rerank candidates.
    let now_unix = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    let since_floor = params
        .since
        .as_deref()
        .and_then(since_window_secs)
        .map(|w| now_unix - w);
    let folder_filter = params
        .folder
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty());
    let project_filter = params
        .project
        .as_deref()
        .map(kb_core::sessions::resolve_project_filter)
        .unwrap_or_default();
    let mut cands: Vec<kb_core::sessions::RecollectCandidate> = Vec::new();
    let mut joined: std::collections::BTreeMap<String, (Hit, SessionOut)> =
        std::collections::BTreeMap::new();
    for (sid, hit) in best {
        let Some(so) = meta.remove(&sid) else {
            continue;
        };
        if let Some(floor) = since_floor {
            if so.started_at < floor {
                continue;
            }
        }
        if let Some(f) = folder_filter {
            if so.folder.as_deref() != Some(f) && so.cwd.as_deref() != Some(f) {
                continue;
            }
        }
        if !project_filter.is_empty()
            && !project_filter.matches(so.project_key.as_deref(), so.cwd.as_deref())
        {
            continue;
        }
        cands.push(kb_core::sessions::RecollectCandidate {
            session_id: sid.clone(),
            rank: hit.rank,
            started_at: so.started_at,
            error_count: so.error_count,
        });
        joined.insert(sid, (hit, so));
    }
    kb_core::sessions::recollect_order(&mut cands, now_unix);
    cands.truncate(limit);

    // 5. Capped set: commit count + staleness + assemble. Resolve the retained
    // (hit, so) pairs first, then fetch every session's commits concurrently
    // (submission-ordered, #28) rather than one serial fan-out per session.
    let retained: Vec<(kb_core::sessions::RecollectCandidate, Hit, SessionOut)> = cands
        .into_iter()
        .filter_map(|c| {
            let (hit, so) = joined.remove(&c.session_id)?;
            Some((c, hit, so))
        })
        .collect();
    let state_ref = &state;
    let mut commit_futs: Vec<super::CorpusFut<'_, Vec<CommitOut>>> =
        Vec::with_capacity(retained.len());
    for (c, _, _) in &retained {
        let sid = c.session_id.clone();
        commit_futs.push(Box::pin(
            async move { fetch_commits(state_ref, &sid).await },
        ));
    }
    // PF-R1 — the operator-configurable `[server] fanout_cap` (default 8,
    // byte-identical to the old hardcoded `super::FANOUT_CAP`).
    let all_commits = super::buffered_join(commit_futs, state.fanout_cap).await;
    let mut sessions = Vec::with_capacity(retained.len());
    for ((c, hit, so), commits) in retained.into_iter().zip(all_commits) {
        let age_days = kb_core::sessions::session_age_days(so.started_at, now_unix);
        sessions.push(RecollectSessionOut {
            session_id: so.session_id,
            kb: hit.kb,
            display_name: so.display_name,
            started_at: so.started_at,
            score: kb_core::sessions::recollect_score(c.rank, so.started_at, now_unix),
            age_days,
            stale: age_days > kb_core::sessions::RECOLLECT_STALE_AFTER_DAYS,
            error_count: so.error_count,
            commit_count: commits.len() as u32,
            committed: !commits.is_empty(),
            first_user_prompt: so.first_user_prompt,
            folder: so.folder,
            summary: hit.summary,
            outcome: so.outcome,
            harness: so.harness,
            project_key: so.project_key,
        });
    }

    Ok(RecollectResponse {
        sessions,
        ms: started.elapsed().as_millis() as u64,
    })
}

/// `GET /api/sessions/{session_id}` — the enrichment + memory id list
/// for one session. 404 when no kb holds a matching row.
pub async fn get(
    State(state): State<Arc<KbHandles>>,
    Path(session_id): Path<String>,
) -> impl IntoResponse {
    let Some(session) = find_session(&state, &session_id).await else {
        return error_to_problem_json(&kb_core::Error::NotFound(format!("session {session_id}")));
    };
    // memory_ids: scan every kb and concatenate. Bounded by
    // PER_SESSION_MEMORY_LIMIT per corpus so a runaway script can't
    // blow out the response.
    let session_id = &session_id;
    let mut futs: Vec<super::CorpusFut<'_, Vec<String>>> = Vec::new();
    for (_, ctx) in state.kbs.iter() {
        futs.push(Box::pin(async move {
            ctx.storage
                .list_docs_with_kb_session(session_id.clone(), PER_SESSION_MEMORY_LIMIT)
                .await
                .map(|rows| rows.into_iter().map(|r| r.id).collect::<Vec<_>>())
                .unwrap_or_default()
        }));
    }
    // PF-R1 — the operator-configurable `[server] fanout_cap` (default 8,
    // byte-identical to the old hardcoded `super::FANOUT_CAP`).
    let mut memory_ids: Vec<String> = super::buffered_join(futs, state.fanout_cap)
        .await
        .into_iter()
        .flatten()
        .collect();
    memory_ids.sort();
    memory_ids.dedup();
    Json(SessionDetailResponse {
        session,
        memory_ids,
    })
    .into_response()
}

/// `GET /api/sessions/{session_id}/memories` — full memory rows for
/// the session, projected through the same slim shape /api/recall
/// uses so the SPA can hand them to the existing memory-row renderer.
pub async fn memories(
    State(state): State<Arc<KbHandles>>,
    Path(session_id): Path<String>,
) -> impl IntoResponse {
    let session_id = &session_id;
    let mut futs: Vec<super::CorpusFut<'_, Vec<MemoryHit>>> = Vec::new();
    for (kb_name, ctx) in state.kbs.iter() {
        futs.push(Box::pin(async move {
            let rows = match ctx
                .storage
                .list_docs_with_kb_session(session_id.clone(), PER_SESSION_MEMORY_LIMIT)
                .await
            {
                Ok(r) => r,
                Err(e) => {
                    tracing::warn!(
                        kb = %kb_name,
                        session_id = %session_id,
                        error = %e,
                        "list_docs_with_kb_session failed",
                    );
                    return Vec::new();
                }
            };
            rows.into_iter()
                .map(|d| {
                    let source_relative = kb_core::paths::doc_rel_path(&d.path, &ctx.source_path);
                    MemoryHit {
                        id: d.id,
                        kb: kb_name.as_str().to_string(),
                        title: d.title,
                        path: d.path,
                        source_relative,
                        mtime_unix: d.mtime_unix,
                        kb_salience: d.kb_salience,
                    }
                })
                .collect()
        }));
    }
    // PF-R1 — the operator-configurable `[server] fanout_cap` (default 8,
    // byte-identical to the old hardcoded `super::FANOUT_CAP`).
    let mut out: Vec<MemoryHit> = super::buffered_join(futs, state.fanout_cap)
        .await
        .into_iter()
        .flatten()
        .collect();
    // Newest-first by mtime; falls back to id ascending for hits that
    // don't carry mtime (rows projected before the column existed).
    out.sort_by(|a, b| {
        b.mtime_unix
            .unwrap_or(i64::MIN)
            .cmp(&a.mtime_unix.unwrap_or(i64::MIN))
            .then_with(|| a.id.cmp(&b.id))
    });
    Json(SessionMemoriesResponse { memories: out })
}

/// `GET /api/sessions/{session_id}/recalls` — MI-W4.2c: every memory this
/// session's `kb-recall` hook actually injected — the PULL side of
/// `memories` above's WRITE side, so `SessionInspector` can show "Memories
/// recalled" beside "Memories produced". Reads the session's OWN
/// `memory_recalls` ledger row via `find_session`'s already-resolved
/// `SessionOut.kb` (the ledger lives with the RECALLING session's kb,
/// invariant #28's note) — a single-kb read, not a fan-out. Each ledger row
/// names a `memory_kb` that may differ from the session's own kb, so
/// display fields (title/source_relative) are resolved with one
/// `get_by_ids` batch PER distinct `memory_kb` the ledger actually
/// mentions — small in practice (a session usually recalls from one or two
/// memory corpora). A `memory_kb` that no longer resolves (unknown kb name,
/// daemon reconfigured since) or an id that no longer resolves (hard
/// deleted) leaves `title`/`source_relative` absent rather than dropping
/// the row — the recall EVENT is the ledger's own truth, independent of
/// whether the memory still exists today. 404 when the session itself
/// doesn't resolve.
pub async fn recalls(
    State(state): State<Arc<KbHandles>>,
    Path(session_id): Path<String>,
) -> impl IntoResponse {
    let Some(session) = find_session(&state, &session_id).await else {
        return error_to_problem_json(&kb_core::Error::NotFound(format!("session {session_id}")));
    };
    let Ok(session_kb) = kb_core::types::KbName::new(&session.kb) else {
        return Json(SessionRecallsResponse {
            recalls: Vec::new(),
        })
        .into_response();
    };
    let Some(ctx) = state.kbs.get(&session_kb) else {
        return Json(SessionRecallsResponse {
            recalls: Vec::new(),
        })
        .into_response();
    };
    let rows = ctx
        .storage
        .memory_recalls_for_session(session_id.clone())
        .await
        .unwrap_or_else(|e| {
            tracing::warn!(
                kb = %session_kb,
                session_id = %session_id,
                error = %e,
                "memory_recalls_for_session failed",
            );
            Vec::new()
        });

    // Batch-resolve display fields, one `get_by_ids` per distinct
    // memory_kb the ledger names.
    let mut ids_by_kb: std::collections::BTreeMap<String, Vec<String>> =
        std::collections::BTreeMap::new();
    for r in &rows {
        ids_by_kb
            .entry(r.memory_kb.clone())
            .or_default()
            .push(r.memory_id.clone());
    }
    let mut resolved: HashMap<(String, String), (String, String)> = HashMap::new();
    for (memory_kb, ids) in ids_by_kb {
        let Ok(kb_name) = kb_core::types::KbName::new(&memory_kb) else {
            continue;
        };
        let Some(mem_ctx) = state.kbs.get(&kb_name) else {
            continue;
        };
        let docs = mem_ctx.storage.get_by_ids(ids).await.unwrap_or_default();
        for d in docs {
            let source_relative = kb_core::paths::doc_rel_path(&d.path, &mem_ctx.source_path);
            resolved.insert(
                (memory_kb.clone(), d.id.clone()),
                (d.title, source_relative),
            );
        }
    }

    let recalls: Vec<SessionRecallHit> = rows
        .into_iter()
        .map(|r| {
            let display = resolved.get(&(r.memory_kb.clone(), r.memory_id.clone()));
            SessionRecallHit {
                kb: r.memory_kb,
                id: r.memory_id,
                title: display.map(|(t, _)| t.clone()),
                source_relative: display.map(|(_, sr)| sr.clone()),
                turn_id: r.turn_id,
                recalled_at: r.recalled_at,
                used: r.used,
                pos: r.pos,
            }
        })
        .collect();

    Json(SessionRecallsResponse { recalls }).into_response()
}

/// `GET /api/sessions/{session_id}/touches` — artifact ids the
/// session's transcript references. Result is `Exact` when every hit
/// came from a literal 12-hex id, `Fuzzy` when any hit came from a
/// source-relative path substring match.
///
/// Cache-first: the LRU is keyed on `(kb, artifact_id, mtime_unix)`,
/// so a re-indexed transcript automatically gets a fresh scan. The
/// transcript read goes through `spawn_blocking` because session HTML
/// can be multi-MB (full Stop-hook capture); doing the read on the
/// tokio worker would block the runtime.
pub async fn touches(
    State(state): State<Arc<KbHandles>>,
    Path(session_id): Path<String>,
) -> impl IntoResponse {
    // 1) Locate the session row. We need (kb, artifact_id, abs path,
    //    mtime_unix). 404 when no kb holds a matching row.
    // FF-D — fan out the session-row lookup; take the first match in BTreeMap
    // (submission) order, exactly as the serial break-on-first did.
    let session_id_ref = &session_id;
    let mut futs: Vec<super::CorpusFut<'_, Option<LocatedSession>>> = Vec::new();
    for (kb_name, ctx) in state.kbs.iter() {
        futs.push(Box::pin(async move {
            match ctx.storage.sessions_get(session_id_ref.clone()).await {
                Ok(Some(row)) => {
                    let abs = ctx.source_path.join(&row.source_relative);
                    Some((
                        kb_name.as_str().to_string(),
                        row.artifact_id,
                        abs,
                        row.ended_at,
                    ))
                }
                Ok(None) => None,
                Err(e) => {
                    tracing::warn!(
                        kb = %kb_name, session_id = %session_id_ref, error = %e,
                        "sessions_get failed in touches handler",
                    );
                    None
                }
            }
        }));
    }
    // PF-R1 — the operator-configurable `[server] fanout_cap` (default 8,
    // byte-identical to the old hardcoded `super::FANOUT_CAP`).
    let located = super::buffered_join(futs, state.fanout_cap)
        .await
        .into_iter()
        .flatten()
        .next();
    let Some((kb, artifact_id, transcript_path, mtime_unix)) = located else {
        return error_to_problem_json(&kb_core::Error::NotFound(format!("session {session_id}")));
    };

    // 2) Cache hit returns immediately.
    if let Some(cached) = state.touches_cache.get(&kb, &artifact_id, mtime_unix) {
        return Json(TouchesResponse {
            artifact_ids: cached.artifact_ids,
            confidence: cached.confidence,
            artifacts: cached.artifacts,
        })
        .into_response();
    }

    // 3) Read the transcript HTML off the tokio worker. Multi-MB
    //    files are realistic; spawn_blocking keeps the runtime clear.
    let read_result =
        tokio::task::spawn_blocking(move || std::fs::read_to_string(&transcript_path)).await;
    let transcript = match read_result {
        Ok(Ok(s)) => s,
        Ok(Err(e)) => {
            tracing::warn!(
                kb = %kb, session_id = %session_id, error = %e,
                "transcript read failed",
            );
            return Json(TouchesResponse {
                artifact_ids: Vec::new(),
                confidence: kb_core::sessions::TouchesConfidence::Exact,
                artifacts: Vec::new(),
            })
            .into_response();
        }
        Err(join_err) => {
            tracing::warn!(
                kb = %kb, session_id = %session_id, error = %join_err,
                "spawn_blocking joined with error",
            );
            return Json(TouchesResponse {
                artifact_ids: Vec::new(),
                confidence: kb_core::sessions::TouchesConfidence::Exact,
                artifacts: Vec::new(),
            })
            .into_response();
        }
    };

    // 4) Build the known-doc index from every kb's gallery row-set memo
    //    (PF-R1; invariant #15's `docs::gallery_snapshot`) instead of an
    //    independent `list_docs(50_000)` actor pull per kb. Slim
    //    projection (id + source_relative only — no body, no embeddings).
    //
    //    NB — this DROPS the previous 50k-per-kb safety cap ("so a
    //    runaway corpus can't OOM the scan"): the shared memo always holds
    //    the FULL corpus (`list_docs(u32::MAX)`, same as every other
    //    gallery-memo consumer — `/docs`, `/facets`, `/edges`, `/resurface`,
    //    `/desk`, `/daycard`). In the common case this is a pure win: the
    //    memo is usually already warm from a higher-traffic route, so this
    //    becomes an `Arc::clone` instead of its own bounded actor round-
    //    trip. On a cold miss against a corpus over 50k docs, this now
    //    pulls the whole corpus rather than a bounded slice — more memory
    //    on that one miss, but MORE docs become "known" to
    //    `extract_touched_ids` below (strictly better match coverage, not
    //    worse — no known doc is ever excluded that the old 50k window
    //    would have included).
    let mut futs: Vec<super::CorpusFut<'_, Vec<kb_core::sessions::KnownDoc>>> = Vec::new();
    for (_, ctx) in state.kbs.iter() {
        futs.push(Box::pin(async move {
            let (rows, _edge_counts) = match crate::routes::docs::gallery_snapshot(ctx).await {
                Ok(v) => v,
                Err(e) => {
                    tracing::warn!(error = %e, "gallery_snapshot failed during touches scan");
                    return Vec::new();
                }
            };
            rows.iter()
                .map(|r| kb_core::sessions::KnownDoc {
                    id: r.doc.id.clone(),
                    source_relative: kb_core::paths::doc_rel_path(&r.doc.path, &ctx.source_path),
                })
                .collect()
        }));
    }
    // PF-R1 — the operator-configurable `[server] fanout_cap` (default 8,
    // byte-identical to the old hardcoded `super::FANOUT_CAP`).
    let known: Vec<kb_core::sessions::KnownDoc> = super::buffered_join(futs, state.fanout_cap)
        .await
        .into_iter()
        .flatten()
        .collect();

    let (artifacts, confidence) = kb_core::sessions::extract_touched_ids(&transcript, &known);
    let artifact_ids: Vec<String> = artifacts.iter().map(|a| a.id.clone()).collect();

    state.touches_cache.put(
        &kb,
        &artifact_id,
        mtime_unix,
        crate::touches_cache::CachedTouches {
            artifact_ids: artifact_ids.clone(),
            confidence,
            artifacts: artifacts.clone(),
        },
    );

    Json(TouchesResponse {
        artifact_ids,
        confidence,
        artifacts,
    })
    .into_response()
}

#[cfg(test)]
mod touches_wire_tests {
    use super::*;
    use kb_core::sessions::{TouchedArtifact, TouchesConfidence};

    // CT-A6 — `TouchesResponse` grew an additive `artifacts` field carrying
    // per-row join-tier confidence (`exact`|`fuzzy`) alongside the untouched
    // `artifact_ids` + aggregate `confidence`. Pinned so the wire shape
    // doesn't silently drift.
    #[test]
    fn touches_response_serializes_per_artifact_confidence() {
        let body = TouchesResponse {
            artifact_ids: vec!["abc123def456".to_string(), "000000000000".to_string()],
            confidence: TouchesConfidence::Fuzzy,
            artifacts: vec![
                TouchedArtifact {
                    id: "abc123def456".to_string(),
                    confidence: TouchesConfidence::Exact,
                },
                TouchedArtifact {
                    id: "000000000000".to_string(),
                    confidence: TouchesConfidence::Fuzzy,
                },
            ],
        };
        let v = serde_json::to_value(&body).unwrap();
        assert_eq!(v["confidence"], serde_json::json!("fuzzy"));
        assert_eq!(v["artifacts"][0]["id"], serde_json::json!("abc123def456"));
        assert_eq!(v["artifacts"][0]["confidence"], serde_json::json!("exact"));
        assert_eq!(v["artifacts"][1]["id"], serde_json::json!("000000000000"));
        assert_eq!(v["artifacts"][1]["confidence"], serde_json::json!("fuzzy"));
    }
}

/// Query for `GET /api/sessions/{id}/export`: which redaction layers to request.
/// A non-loopback client always gets at least `secrets` forced on (invariant
/// #4) — the raw transcript must never leave a public bind unredacted.
#[derive(Debug, Default, Deserialize)]
pub struct ExportQuery {
    /// Comma-separated: `secrets`, `paths`, `entropy`, or `all`.
    scrub: Option<String>,
}

fn parse_scrub_layers(spec: &Option<String>) -> kb_core::session_scrub::ScrubOptions {
    let mut o = kb_core::session_scrub::ScrubOptions::default();
    if let Some(s) = spec {
        for part in s.split(',') {
            match part.trim() {
                "secrets" => o.secrets = true,
                "paths" => o.paths = true,
                "entropy" => o.entropy = true,
                "all" => {
                    o.secrets = true;
                    o.paths = true;
                    o.entropy = true;
                }
                _ => {}
            }
        }
    }
    o
}

/// Keep only filename-safe chars for the download name (the canonical id is a
/// UUID, so this is a no-op in practice).
fn sanitize_filename_id(s: &str) -> String {
    s.chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect()
}

/// `GET /api/sessions/{session_id}/export` — stream a portable
/// `<sid>.kbsession.zip` (manifest.json + session.jsonl) for cross-machine
/// `claude -r` resume via `kb sessions pull`. Resolves the NEWEST capture
/// (invariant #11) by cross-kb fan-out, recovers the raw transcript, and builds
/// the SAME `kb_core::session_bundle` shape the CLI export produces.
///
/// **Fail-closed redaction (invariant #4/#11):** the raw transcript is
/// sensitive; a non-loopback client ALWAYS gets the `secrets` scrub floor
/// forced on (plus any `?scrub=` layers), so a public daemon never streams an
/// unredacted transcript. `x-kb-session-scrubbed` / `x-kb-redactions` headers
/// make the posture auditable.
pub async fn export(
    State(state): State<Arc<KbHandles>>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Path(session_id): Path<String>,
    Query(q): Query<ExportQuery>,
) -> Response {
    // Locate the newest-capture row across all kbs (BTreeMap order, first-wins),
    // capturing everything the bundle needs. `(kb, abs_path, session_id,
    // started_at, source_relative)`.
    type Located = (String, std::path::PathBuf, String, i64, String);
    let session_id_ref = &session_id;
    let mut futs: Vec<super::CorpusFut<'_, Option<Located>>> = Vec::new();
    for (kb_name, ctx) in state.kbs.iter() {
        futs.push(Box::pin(async move {
            match ctx.storage.sessions_get(session_id_ref.clone()).await {
                Ok(Some(row)) => {
                    let abs = ctx.source_path.join(&row.source_relative);
                    Some((
                        kb_name.as_str().to_string(),
                        abs,
                        row.session_id,
                        row.started_at,
                        row.source_relative,
                    ))
                }
                Ok(None) => None,
                Err(e) => {
                    tracing::warn!(kb = %kb_name, session_id = %session_id_ref, error = %e, "sessions_get failed in export");
                    None
                }
            }
        }));
    }
    // PF-R1 — the operator-configurable `[server] fanout_cap` (default 8,
    // byte-identical to the old hardcoded `super::FANOUT_CAP`).
    let located = super::buffered_join(futs, state.fanout_cap)
        .await
        .into_iter()
        .flatten()
        .next();
    let Some((kb, abs, row_sid, row_started, source_rel)) = located else {
        return error_to_problem_json(&kb_core::Error::NotFound(format!("session {session_id}")))
            .into_response();
    };

    // Read the capture HTML off the tokio worker (multi-MB), recover the JSONL.
    let read = tokio::task::spawn_blocking(move || std::fs::read_to_string(&abs)).await;
    let html = match read {
        Ok(Ok(s)) => s,
        _ => {
            return error_to_problem_json(&kb_core::Error::NotFound(format!(
                "capture unreadable for session {session_id}"
            )))
            .into_response()
        }
    };
    let Some(jsonl) = kb_core::sessions::recover_jsonl_from_capture(&html) else {
        return error_to_problem_json(&kb_core::Error::NotFound(format!(
            "no transcript in capture for session {session_id}"
        )))
        .into_response();
    };

    // Derive via the shared kb-core helpers (byte-compatible with the CLI).
    let canonical_id =
        kb_core::session_bundle::first_transcript_field(&jsonl, "sessionId").unwrap_or(row_sid);
    let started_at = kb_core::session_bundle::earliest_event_ts(&jsonl).unwrap_or(row_started);
    let cc_version = kb_core::session_bundle::first_transcript_field(&jsonl, "version");
    let origin = kb_core::session_bundle::BundleOrigin {
        kb: Some(kb),
        source_relative: Some(source_rel),
        exporter_version: Some(env!("CARGO_PKG_VERSION").to_string()),
    };

    // Scrub: honour `?scrub=`, and FORCE the secrets floor on a non-loopback
    // client (#4/#11). No git provenance server-side (the daemon box has no
    // user repos).
    let mut scrub = parse_scrub_layers(&q.scrub);
    if crate::scrub::looks_non_loopback(Some(peer.ip()), &headers, &state.origin.trusted_proxies) {
        scrub.secrets = true;
    }
    let assembled = kb_core::session_bundle::assemble_bundle(
        &jsonl,
        canonical_id.clone(),
        Some(started_at),
        cc_version,
        origin,
        None,
        None,
        &scrub,
    );

    // Zip manifest.json + session.jsonl (deflate — the download-route pattern).
    use std::io::{Cursor, Write};
    use zip::write::{SimpleFileOptions, ZipWriter};
    let mut zip = ZipWriter::new(Cursor::new(Vec::<u8>::new()));
    let opts = SimpleFileOptions::default().compression_method(zip::CompressionMethod::Deflated);
    let zipped: zip::result::ZipResult<()> = (|| {
        zip.start_file(kb_core::session_bundle::MANIFEST_ENTRY, opts)?;
        zip.write_all(assembled.manifest_json.as_bytes())?;
        zip.start_file(kb_core::session_bundle::TRANSCRIPT_ENTRY, opts)?;
        zip.write_all(assembled.transcript.as_bytes())?;
        Ok(())
    })();
    let buf = match zipped.and_then(|_| zip.finish()) {
        Ok(cur) => cur.into_inner(),
        Err(e) => {
            tracing::warn!(session_id = %session_id, error = %e, "session bundle zip failed");
            return (StatusCode::INTERNAL_SERVER_ERROR, "bundle assembly failed").into_response();
        }
    };

    let filename = format!("{}.kbsession.zip", sanitize_filename_id(&canonical_id));
    let mut resp = (StatusCode::OK, buf).into_response();
    let h = resp.headers_mut();
    h.insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("application/zip"),
    );
    if let Ok(v) = HeaderValue::from_str(&super::docs::attachment_disposition(&filename)) {
        h.insert(header::CONTENT_DISPOSITION, v);
    }
    h.insert(
        "x-kb-session-scrubbed",
        HeaderValue::from_static(if assembled.manifest.scrubbed {
            "1"
        } else {
            "0"
        }),
    );
    if let Ok(v) = HeaderValue::from_str(&assembled.report.total.to_string()) {
        h.insert("x-kb-redactions", v);
    }
    resp
}

// --- W3.R-b — the session replay timeline -----------------------------------

/// Only artifacts that actually carry a ranged beat get their source read for
/// heading anchors, and never more than this many per request — a pathological
/// transcript must not turn one replay into hundreds of file reads.
const REPLAY_HEADING_SOURCE_CAP: usize = 64;

/// Query for `GET /api/sessions/{session_id}/replay`.
#[derive(Debug, Default, Deserialize)]
pub struct ReplayQuery {
    /// Keep only beats that resolved to this artifact id.
    artifact: Option<String>,
    /// R7/S6 — serve-window offset into the (post-`?artifact=`) matched beat
    /// list. Applied BEFORE `limit`. `None` = 0 (the head, behavior-compatible
    /// with pre-windowing clients on any session short enough that the old
    /// `REPLAY_MAX_EVENTS=2,000` compute cap never bit).
    from_seq: Option<usize>,
    /// Cap on returned beats, applied AFTER `?artifact=` and `?from_seq=`.
    /// This is a response WINDOW size, deliberately NOT
    /// `ReplayOptions::max_events` (now a 50k safety ceiling, not a serve
    /// window — R7) — the cached timeline is always built at the kb-core
    /// default so two clients windowing the same capture differently still
    /// share one build. `None` defaults to `REPLAY_DEFAULT_WINDOW`.
    limit: Option<usize>,
    /// Same comma-separated redaction-layer spec as `/export`
    /// (`secrets`,`paths`,`entropy`,`all`). A non-loopback client always gets
    /// at least `secrets` on top of whatever it asked for.
    scrub: Option<String>,
}

/// R7/S6 — the default `?limit=` when the caller doesn't set one: matches the
/// OLD compute cap so a short session's response is byte-shape-compatible
/// with pre-windowing clients (a session under 2,000 beats returns its full
/// timeline either way); a long session now serves its HEAD window by
/// default rather than silently losing its tail at compute time.
const REPLAY_DEFAULT_WINDOW: usize = 2_000;

/// The cache-key discriminator for a redaction posture. Load-bearing for
/// invariant #4 — see `crate::replay_cache`.
fn scrub_tag(o: &kb_core::session_scrub::ScrubOptions) -> String {
    let mut t = String::new();
    if o.secrets {
        t.push('s');
    }
    if o.paths {
        t.push('p');
    }
    if o.entropy {
        t.push('e');
    }
    t
}

/// One replay beat plus its corpus resolution.
///
/// The `beat` is the kb-core value VERBATIM (same bytes the CLI prints), so
/// the SPA and `kb sessions replay --json` can never drift. Everything the
/// daemon adds is resolution: which artifact the beat's raw path is, and which
/// heading a ranged read landed under.
///
/// A beat whose path is not in any corpus keeps its raw `beat.path` and
/// resolves to `None` here — that is CORRECT (the agent read a source file, a
/// config, `/etc/hosts`), and such a beat must be rendered as a plain path,
/// never dropped.
#[cfg_attr(feature = "ts-export", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Clone, Serialize)]
pub struct ReplayBeatOut {
    pub beat: kb_core::sessions::replay::ReplayBeat,
    /// Owning kb, when the beat's path resolved to an indexed artifact.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts-export", ts(optional))]
    pub kb: Option<String>,
    /// Artifact id, when resolved. Canonical join: path → `resolve_corpus_path`
    /// → artifact id → storage. NEVER the lance `kb_session` column (#11).
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts-export", ts(optional))]
    pub artifact_id: Option<String>,
    /// Source-relative path of the resolved artifact (permalink material).
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts-export", ts(optional))]
    pub source_relative: Option<String>,
    /// Nearest preceding heading id for a beat carrying a `line_range` — the
    /// value the iframe runtime accepts as `kb:scroll-to-id`.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts-export", ts(optional))]
    pub heading_slug: Option<String>,
}

/// The cached, fully-resolved replay for one capture + one redaction posture.
/// `?artifact=` / `?limit=` are pure filters applied to this on the way out,
/// so they never fragment the cache.
#[derive(Debug, Clone)]
pub struct ResolvedReplay {
    pub kb: String,
    pub artifact_id: String,
    pub beats: Vec<ReplayBeatOut>,
    pub started_at: Option<i64>,
    pub ended_at: Option<i64>,
    pub duration_secs: i64,
    pub records: usize,
    pub metadata_skipped: usize,
    pub out_of_order: usize,
    pub collapsed: usize,
    pub truncated: bool,
    pub dropped: usize,
    pub scrubbed: bool,
    pub redactions: u32,
}

/// `GET /api/sessions/{session_id}/replay` — the session-replay/1 timeline.
#[cfg_attr(feature = "ts-export", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Serialize)]
pub struct SessionReplayResponse {
    /// Always `session-replay/1` — a consumer may refuse a shape it doesn't know.
    pub grammar: String,
    pub session_id: String,
    /// The kb holding the capture the timeline was built from.
    pub kb: String,
    /// The NEWEST capture's artifact id (invariant #11).
    pub artifact_id: String,
    pub beats: Vec<ReplayBeatOut>,
    /// First / last beat instant (`null` on an empty timeline) — always
    /// present on the wire, mirroring `ReplayTimeline`.
    pub started_at: Option<i64>,
    pub ended_at: Option<i64>,
    pub duration_secs: i64,
    pub records: usize,
    pub metadata_skipped: usize,
    pub out_of_order: usize,
    pub collapsed: usize,
    /// True when kb-core's `max_events` cap bit while building the timeline.
    pub truncated: bool,
    pub dropped: usize,
    /// Beats the timeline holds before `?artifact=` / `?from_seq=` /
    /// `?limit=` filtering.
    pub total_beats: usize,
    /// Beats matching `?artifact=` before `?from_seq=`/`?limit=` — so a
    /// client can see that its window is a window.
    pub matched: usize,
    /// R7/S6 — the serve-window actually returned, over the `matched` set
    /// (post-`?artifact=`, pre-slice). `returned == beats.len()`; present
    /// even when `?from_seq=`/`?limit=` were both absent (the default
    /// window), so a client always knows where in the timeline it's looking.
    pub window: ReplayWindow,
    /// Whether any redaction layer ran (mirrors `x-kb-session-scrubbed`).
    pub scrubbed: bool,
    /// How many redactions the scrub pass applied (mirrors `x-kb-redactions`).
    pub redactions: u32,
}

#[cfg_attr(feature = "ts-export", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Serialize)]
pub struct ReplayWindow {
    pub from_seq: usize,
    pub returned: usize,
    /// The `matched` count (post-`?artifact=`) this window slices — repeated
    /// here (not just at the response's top level) so `window` alone is
    /// enough to render a pager ("beats {from_seq}–{from_seq+returned} of
    /// {total}").
    pub total: usize,
}

/// Pure: apply `?artifact=` then the `?from_seq=`/`?limit=` serve-window
/// (memo R7 — kb-core computes the FULL timeline; this is where it's
/// sliced). Returns `(window_beats, matched, window_meta)` where `matched`
/// counts everything that passed `?artifact=` (pre-slice), so a windowed
/// response is always visible as such.
fn filter_replay_beats<'a>(
    beats: &'a [ReplayBeatOut],
    artifact: Option<&str>,
    from_seq: usize,
    limit: usize,
) -> (Vec<&'a ReplayBeatOut>, usize, ReplayWindow) {
    let matched: Vec<&ReplayBeatOut> = beats
        .iter()
        .filter(|b| match artifact {
            Some(a) => b.artifact_id.as_deref() == Some(a),
            None => true,
        })
        .collect();
    let total = matched.len();
    let window: Vec<&ReplayBeatOut> = matched.into_iter().skip(from_seq).take(limit).collect();
    let meta = ReplayWindow {
        from_seq,
        returned: window.len(),
        total,
    };
    (window, total, meta)
}

/// Per-raw-path resolution, computed once per distinct path in the timeline.
#[derive(Debug, Clone, Default)]
struct BeatResolution {
    kb: Option<String>,
    artifact_id: Option<String>,
    source_relative: Option<String>,
    /// Canonical absolute path of the resolved artifact — the file the
    /// heading pass reads. Never serialised.
    doc_path: Option<String>,
}

/// Resolve every beat's raw path to `(kb, artifact_id, source_relative)` and,
/// for a beat carrying a `line_range`, to the nearest preceding heading id.
///
/// Two passes, both bounded:
///
/// 1. **Pure** `resolve_corpus_path(path, cwd, mounts)` per DISTINCT path —
///    no I/O, and it yields the same source-relative id the indexer assigned
///    (invariant #27). Confirmation is `get_by_id` against the OWNING kb's
///    storage: the canonical artifact_id→storage join. The lance `kb_session`
///    column is a hint and is never consulted here (invariant #11).
/// 2. **Heading anchors** for the artifacts that a ranged beat actually
///    touched, read off the tokio worker.
async fn resolve_replay_beats(
    state: &Arc<KbHandles>,
    cwd: Option<&str>,
    beats: Vec<kb_core::sessions::replay::ReplayBeat>,
) -> Vec<ReplayBeatOut> {
    let mounts = kb_core::sessions::corpus_mounts();

    // Pass 1a — pure path → (kb, id), deduped. BTreeMap keeps the fan-out
    // submission order deterministic (invariant #28).
    let mut candidates: std::collections::BTreeMap<String, (String, String)> =
        std::collections::BTreeMap::new();
    for b in &beats {
        let Some(p) = b.path.as_deref() else { continue };
        if candidates.contains_key(p) {
            continue;
        }
        let (in_corpus, kb, id) = kb_core::sessions::resolve_corpus_path(p, cwd, &mounts);
        if let (true, Some(kb), Some(id)) = (in_corpus, kb, id) {
            candidates.insert(p.to_string(), (kb, id));
        }
    }

    // Pass 1b — confirm each candidate exists, concurrently in submission
    // order. A miss (deleted / not yet indexed) folds to "unresolved", which
    // renders as the plain raw path.
    let keys: Vec<String> = candidates.keys().cloned().collect();
    let state_ref = state;
    let mut futs: Vec<super::CorpusFut<'_, Option<(String, String, String)>>> =
        Vec::with_capacity(keys.len());
    for key in &keys {
        let (kb, id) = candidates.get(key).cloned().expect("key from candidates");
        futs.push(Box::pin(async move {
            let (_, ctx) = state_ref.kbs.iter().find(|(n, _)| n.as_str() == kb)?;
            let doc = ctx.storage.get_by_id(id).await.ok().flatten()?;
            Some((
                kb,
                doc.id,
                kb_core::paths::doc_rel_path(&doc.path, &ctx.source_path),
            ))
        }));
    }
    // PF-R1 — the operator-configurable `[server] fanout_cap` (default 8,
    // byte-identical to the old hardcoded `super::FANOUT_CAP`).
    let confirmed = super::buffered_join(futs, state.fanout_cap).await;
    let mut resolved: std::collections::BTreeMap<String, BeatResolution> =
        std::collections::BTreeMap::new();
    for (key, hit) in keys.iter().zip(confirmed) {
        let Some((kb, id, source_relative)) = hit else {
            continue;
        };
        let doc_path = state
            .kbs
            .iter()
            .find(|(n, _)| n.as_str() == kb)
            .map(|(_, ctx)| {
                ctx.source_path
                    .join(&source_relative)
                    .to_string_lossy()
                    .to_string()
            });
        resolved.insert(
            key.clone(),
            BeatResolution {
                kb: Some(kb),
                artifact_id: Some(id),
                source_relative: Some(source_relative),
                doc_path,
            },
        );
    }

    // Pass 2 — heading anchors, only for artifacts a ranged beat touched.
    let mut heading_sources: Vec<String> = Vec::new();
    for b in &beats {
        if b.line_range.is_none() {
            continue;
        }
        let Some(p) = b.path.as_deref() else { continue };
        let Some(r) = resolved.get(p) else { continue };
        let Some(dp) = r.doc_path.as_deref() else {
            continue;
        };
        if !heading_sources.iter().any(|s| s == dp) {
            heading_sources.push(dp.to_string());
            if heading_sources.len() >= REPLAY_HEADING_SOURCE_CAP {
                break;
            }
        }
    }
    let anchors: std::collections::BTreeMap<String, Vec<kb_core::headings::HeadingAnchor>> =
        if heading_sources.is_empty() {
            std::collections::BTreeMap::new()
        } else {
            tokio::task::spawn_blocking(move || {
                heading_sources
                    .into_iter()
                    .filter_map(|p| {
                        let src = std::fs::read_to_string(&p).ok()?;
                        Some((p, kb_core::headings::heading_anchors(&src)))
                    })
                    .collect()
            })
            .await
            .unwrap_or_default()
        };

    beats
        .into_iter()
        .map(|beat| {
            let r = beat
                .path
                .as_deref()
                .and_then(|p| resolved.get(p))
                .cloned()
                .unwrap_or_default();
            let heading_slug = match (beat.line_range, r.doc_path.as_deref()) {
                (Some((start, _)), Some(dp)) => anchors
                    .get(dp)
                    .and_then(|a| kb_core::headings::slug_for_line(a, start))
                    .map(|s| s.to_string()),
                _ => None,
            };
            ReplayBeatOut {
                beat,
                kb: r.kb,
                artifact_id: r.artifact_id,
                source_relative: r.source_relative,
                heading_slug,
            }
        })
        .collect()
}

/// Build the HTTP response from a (cached or fresh) resolved replay.
fn replay_response(session_id: &str, resolved: &ResolvedReplay, q: &ReplayQuery) -> Response {
    let from_seq = q.from_seq.unwrap_or(0);
    let limit = q.limit.unwrap_or(REPLAY_DEFAULT_WINDOW);
    let (window, matched, window_meta) =
        filter_replay_beats(&resolved.beats, q.artifact.as_deref(), from_seq, limit);
    let body = SessionReplayResponse {
        grammar: kb_core::sessions::replay::REPLAY_GRAMMAR.to_string(),
        session_id: session_id.to_string(),
        kb: resolved.kb.clone(),
        artifact_id: resolved.artifact_id.clone(),
        beats: window.into_iter().cloned().collect(),
        started_at: resolved.started_at,
        ended_at: resolved.ended_at,
        duration_secs: resolved.duration_secs,
        records: resolved.records,
        metadata_skipped: resolved.metadata_skipped,
        out_of_order: resolved.out_of_order,
        collapsed: resolved.collapsed,
        truncated: resolved.truncated,
        dropped: resolved.dropped,
        total_beats: resolved.beats.len(),
        matched,
        window: window_meta,
        scrubbed: resolved.scrubbed,
        redactions: resolved.redactions,
    };
    let mut resp = Json(body).into_response();
    let h = resp.headers_mut();
    h.insert(
        "x-kb-session-scrubbed",
        HeaderValue::from_static(if resolved.scrubbed { "1" } else { "0" }),
    );
    if let Ok(v) = HeaderValue::from_str(&resolved.redactions.to_string()) {
        h.insert("x-kb-redactions", v);
    }
    resp
}

/// `GET /api/sessions/{session_id}/replay` — replay one captured session beat
/// by beat: prompts, tool calls, edits, commits, decisions, on the
/// transcript's own clock (`session-replay/1`).
///
/// Shaped exactly like [`export`], because it has exactly `export`'s hard
/// parts:
///
/// * the NEWEST capture is located by cross-kb fan-out through
///   `sessions_get` (invariant #11 — one long session accrues many `sessions`
///   rows and the newest is the SUPERSET; a bare session_id join would read an
///   older capture);
/// * the capture HTML (multi-MB; real ones reach ~33 MB) is read off the tokio
///   worker via `spawn_blocking`, then `recover_jsonl_from_capture`'d;
/// * **the redaction floor fails CLOSED** — the replay wire carries verbatim
///   prompt text, assistant prose and Edit snippets, which is precisely what
///   `/export` fail-closes on, so a non-loopback client always gets at least
///   `secrets` (invariant #4). The transcript is scrubbed BEFORE the timeline
///   is built, so every derived text field goes through the same
///   `scrub_transcript` path `/export` uses — there is no field a later beat
///   kind could add that would silently bypass it. `x-kb-session-scrubbed` /
///   `x-kb-redactions` make the posture auditable, mirroring `/export`.
/// * every per-corpus future is folded, never `?`-propagated (invariant #28).
///
/// The resolved timeline is memoised in `state.replay_cache` (process-local,
/// NOT a storage message, never bumps the index generation) keyed on
/// `(kb, artifact_id, mtime_unix, scrub-posture)`.
pub async fn replay(
    State(state): State<Arc<KbHandles>>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Path(session_id): Path<String>,
    Query(q): Query<ReplayQuery>,
) -> Response {
    // 1) Locate the NEWEST capture across all kbs (BTreeMap order, first-wins).
    //    `(kb, artifact_id, abs_path, mtime_unix, cwd)`.
    type Located = (String, String, std::path::PathBuf, i64, Option<String>);
    let session_id_ref = &session_id;
    let mut futs: Vec<super::CorpusFut<'_, Option<Located>>> = Vec::new();
    for (kb_name, ctx) in state.kbs.iter() {
        futs.push(Box::pin(async move {
            match ctx.storage.sessions_get(session_id_ref.clone()).await {
                Ok(Some(row)) => {
                    let abs = ctx.source_path.join(&row.source_relative);
                    Some((
                        kb_name.as_str().to_string(),
                        row.artifact_id,
                        abs,
                        row.ended_at,
                        row.cwd,
                    ))
                }
                Ok(None) => None,
                Err(e) => {
                    tracing::warn!(kb = %kb_name, session_id = %session_id_ref, error = %e, "sessions_get failed in replay");
                    None
                }
            }
        }));
    }
    // PF-R1 — the operator-configurable `[server] fanout_cap` (default 8,
    // byte-identical to the old hardcoded `super::FANOUT_CAP`).
    let located = super::buffered_join(futs, state.fanout_cap)
        .await
        .into_iter()
        .flatten()
        .next();
    let Some((kb, artifact_id, abs, mtime_unix, cwd)) = located else {
        return error_to_problem_json(&kb_core::Error::NotFound(format!("session {session_id}")))
            .into_response();
    };

    // 2) Redaction posture — `?scrub=` plus the forced secrets floor on a
    //    non-loopback client (#4). Copied verbatim from `export`.
    let mut scrub = parse_scrub_layers(&q.scrub);
    if crate::scrub::looks_non_loopback(Some(peer.ip()), &headers, &state.origin.trusted_proxies) {
        scrub.secrets = true;
    }
    let tag = scrub_tag(&scrub);

    // 3) Cache: same capture + same posture ⇒ same bytes.
    if let Some(hit) = state.replay_cache.get(&kb, &artifact_id, mtime_unix, &tag) {
        return replay_response(&session_id, &hit, &q);
    }

    // 4) Read the capture off the tokio worker, recover the transcript.
    let read = tokio::task::spawn_blocking(move || std::fs::read_to_string(&abs)).await;
    let html = match read {
        Ok(Ok(s)) => s,
        _ => {
            return error_to_problem_json(&kb_core::Error::NotFound(format!(
                "capture unreadable for session {session_id}"
            )))
            .into_response()
        }
    };
    let Some(jsonl) = kb_core::sessions::recover_jsonl_from_capture(&html) else {
        return error_to_problem_json(&kb_core::Error::NotFound(format!(
            "no transcript in capture for session {session_id}"
        )))
        .into_response();
    };

    // 5) Scrub FIRST (so every derived field is covered), then extract. Both
    //    steps are pure + CPU-bound over a multi-MB string: keep them off the
    //    tokio worker.
    let jsonl_len = jsonl.len();
    let built = tokio::task::spawn_blocking(move || {
        let (text, report) = if scrub.any() {
            kb_core::session_scrub::scrub_transcript(&jsonl, &scrub)
        } else {
            (jsonl, kb_core::session_scrub::ScrubReport::default())
        };
        let tl = kb_core::sessions::replay::replay_timeline(
            &text,
            &kb_core::sessions::replay::ReplayOptions::default(),
        );
        (tl, report)
    })
    .await;
    let (timeline, report) = match built {
        Ok(v) => v,
        Err(e) => {
            tracing::warn!(session_id = %session_id, bytes = jsonl_len, error = %e, "replay extraction joined with error");
            return error_to_problem_json(&kb_core::Error::Internal(anyhow::anyhow!(
                "replay extraction failed: {e}"
            )))
            .into_response();
        }
    };
    let scrubbed = report.total > 0;
    let redactions = report.total;

    // 6) Resolve each beat's raw path against the corpus.
    let beats = resolve_replay_beats(&state, cwd.as_deref(), timeline.beats).await;
    let resolved = Arc::new(ResolvedReplay {
        kb: kb.clone(),
        artifact_id: artifact_id.clone(),
        beats,
        started_at: timeline.started_at,
        ended_at: timeline.ended_at,
        duration_secs: timeline.duration_secs,
        records: timeline.records,
        metadata_skipped: timeline.metadata_skipped,
        out_of_order: timeline.out_of_order,
        collapsed: timeline.collapsed,
        truncated: timeline.truncated,
        dropped: timeline.dropped,
        scrubbed,
        redactions,
    });
    state
        .replay_cache
        .put(&kb, &artifact_id, mtime_unix, &tag, resolved.clone());
    replay_response(&session_id, &resolved, &q)
}

// --- W2/R1 — session-view/1 (`/view`) + the decoded-JSONL twin (`/raw`) ----

/// Locate the NEWEST capture for `session_id` across every kb (invariant
/// #11): `(kb, artifact_id, abs_path, mtime_unix)`. Shared by `/view` and
/// `/raw` — both need exactly this; `/replay` additionally needs `cwd` for
/// beat-corpus resolution, so it keeps its own inline fan-out. Mirrors
/// `mtime_unix` = `row.ended_at` — the same proxy `replay`/`export` use (the
/// `sessions` row carries no real fs mtime; `ended_at` bumps on every
/// re-capture, which is exactly the cache-invalidation signal wanted).
async fn locate_capture(
    state: &Arc<KbHandles>,
    session_id: &str,
) -> Option<(String, String, std::path::PathBuf, i64)> {
    type Located = (String, String, std::path::PathBuf, i64);
    let mut futs: Vec<super::CorpusFut<'_, Option<Located>>> = Vec::new();
    for (kb_name, ctx) in state.kbs.iter() {
        futs.push(Box::pin(async move {
            match ctx.storage.sessions_get(session_id.to_string()).await {
                Ok(Some(row)) => {
                    let abs = ctx.source_path.join(&row.source_relative);
                    Some((kb_name.as_str().to_string(), row.artifact_id, abs, row.ended_at))
                }
                Ok(None) => None,
                Err(e) => {
                    tracing::warn!(kb = %kb_name, session_id = %session_id, error = %e, "sessions_get failed");
                    None
                }
            }
        }));
    }
    // PF-R1 — the operator-configurable `[server] fanout_cap` (default 8,
    // byte-identical to the old hardcoded `super::FANOUT_CAP`).
    super::buffered_join(futs, state.fanout_cap)
        .await
        .into_iter()
        .flatten()
        .next()
}

/// PF-R1 — how many of the newest turns `GET …/view` returns when the
/// caller doesn't ask for a specific `?turns=` window. No prior ratified
/// constant existed for a turns window (checked every `sessions::view`/
/// `sessions.rs` const and `docs/architecture-invariants.md` §11) —
/// `REPLAY_DEFAULT_WINDOW` (2,000) windows `/replay`'s BEAT count from the
/// HEAD; a turn is a full rendered card (prose + tool detail, much heavier
/// per-item than a beat) and the default read pattern here is "what just
/// happened", so this windows from the TAIL instead. 50 is a generous
/// "what just happened" read without transferring a whole long session by
/// default; the escape hatch is `?turns=all`.
const VIEW_DEFAULT_TAIL_TURNS: usize = 50;

/// Query for `GET /api/sessions/{sid}/view`.
#[derive(Debug, Default, Deserialize)]
pub struct ViewQuery {
    /// Comma-separated subset of `header,turns,outline,tasks,subagents,
    /// minimap,stats`. Absent = every section (the full IR). `turns`
    /// implicitly carries `side_lanes` (meaningless without the turns they
    /// reference).
    fields: Option<String>,
    /// PF-R1 — how much of `turns`/`side_lanes` to return, applied to the
    /// cached FULL view (mirrors replay's post-cache `?limit=`, so two
    /// clients windowing the same capture differently still share one
    /// build). Grammar (`parse_turns_spec`): absent = the last
    /// `VIEW_DEFAULT_TAIL_TURNS` turns (the new default — a no-projection
    /// GET no longer returns the whole transcript); `all` = every turn
    /// (the escape hatch, byte-identical `turns`/`side_lanes` to the
    /// pre-PF-R1 no-param response); a bare unsigned int `k` = the last
    /// `k` turns; `A..B` = the W2/R1 ordinal-range window, unchanged. A
    /// malformed spec degrades to the default TAIL window — never to
    /// `all` (the wire-safety rule: an unrecognised `?turns=` must not
    /// silently hand back the whole transcript) and never a 400.
    turns: Option<String>,
    /// Same comma-separated redaction-layer spec as `/export`/`/replay`.
    scrub: Option<String>,
}

/// Which [`kb_core::sessions::view::SessionView`] sections `?fields=` asked
/// for. `all()` (the no-param default) mirrors the un-projected wire shape.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct ViewFields {
    header: bool,
    turns: bool,
    outline: bool,
    tasks: bool,
    subagents: bool,
    minimap: bool,
    stats: bool,
}

impl ViewFields {
    fn all() -> Self {
        Self {
            header: true,
            turns: true,
            outline: true,
            tasks: true,
            subagents: true,
            minimap: true,
            stats: true,
        }
    }

    fn parse(spec: &str) -> Self {
        let mut f = Self {
            header: false,
            turns: false,
            outline: false,
            tasks: false,
            subagents: false,
            minimap: false,
            stats: false,
        };
        for part in spec.split(',') {
            match part.trim() {
                "header" => f.header = true,
                "turns" => f.turns = true,
                "outline" => f.outline = true,
                "tasks" => f.tasks = true,
                "subagents" => f.subagents = true,
                "minimap" => f.minimap = true,
                "stats" => f.stats = true,
                _ => {}
            }
        }
        f
    }

    fn from_query(spec: &Option<String>) -> Self {
        match spec.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
            Some(s) => Self::parse(s),
            None => Self::all(),
        }
    }
}

/// `N` or `A..B` turn-ordinal window, inclusive, order-independent (`5..2`
/// behaves like `2..5`; bare `5` is `5..5`). `None` on a malformed spec
/// (the caller then serves the unwindowed section rather than erroring —
/// a mistyped `?turns=` degrades to "no window", never a 400). Shared
/// grammar lives in `kb_core::sessions::view::parse_turn_window`.
fn parse_turn_window(spec: &str) -> Option<(u32, u32)> {
    kb_core::sessions::view::parse_turn_window(spec)
}

/// PF-R1 — how `?turns=` windows the response's `turns`/`side_lanes`
/// sections (and the `turns_total`/`turns_returned` metadata, which is
/// always computed regardless of `?fields=`). See [`ViewQuery::turns`]'s
/// doc comment for the full grammar.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TurnsSpec {
    /// Every turn — `?turns=all`, or an explicit ordinal range that
    /// happens to cover the whole session (not specially detected; that
    /// just falls out of `Ordinal`'s filter matching everything).
    All,
    /// The last `n` turns by position — the PF-R1 default
    /// (`VIEW_DEFAULT_TAIL_TURNS`) and a bare `?turns=<n>`.
    Tail(usize),
    /// `?turns=A..B`, the W2/R1 ordinal-range window (unchanged).
    Ordinal(u32, u32),
}

/// Parse `?turns=`. See [`ViewQuery::turns`] for the grammar; pure, so the
/// default/all/tail/ordinal/malformed cases are each directly
/// unit-testable (`turns_spec_tests`, below) without a daemon.
fn parse_turns_spec(spec: Option<&str>) -> TurnsSpec {
    match spec.map(str::trim).filter(|s| !s.is_empty()) {
        None => TurnsSpec::Tail(VIEW_DEFAULT_TAIL_TURNS),
        Some("all") => TurnsSpec::All,
        Some(s) => match s.parse::<usize>() {
            Ok(n) => TurnsSpec::Tail(n),
            Err(_) => match parse_turn_window(s) {
                Some((a, b)) => TurnsSpec::Ordinal(a, b),
                // Malformed — degrade to the default window, never to
                // `All` (the wire-safety rule) and never a 400.
                None => TurnsSpec::Tail(VIEW_DEFAULT_TAIL_TURNS),
            },
        },
    }
}

/// PF-R1 — apply a parsed `?turns=` spec to the FULL turn list, transcript
/// order preserved. Mirrors `filter_replay_beats`'s "the slice travels
/// with an honest count" discipline: the caller derives `turns_returned`
/// from `.len()` rather than this function reporting it separately.
fn window_turns_by_spec(
    turns: &[kb_core::sessions::view::Turn],
    spec: TurnsSpec,
) -> Vec<&kb_core::sessions::view::Turn> {
    match spec {
        TurnsSpec::All => turns.iter().collect(),
        TurnsSpec::Tail(n) => {
            let start = turns.len().saturating_sub(n);
            turns[start..].iter().collect()
        }
        TurnsSpec::Ordinal(a, b) => turns
            .iter()
            .filter(|t| t.ordinal >= a && t.ordinal <= b)
            .collect(),
    }
}

/// Keep only side lanes referencing at least one kept turn — one membership
/// test regardless of which `TurnsSpec` variant produced `kept_ids` (with
/// `TurnsSpec::All`, every turn's id is a member, so this is equivalent to
/// the pre-PF-R1 "no filtering at all" behavior).
fn window_side_lanes(
    lanes: &[kb_core::sessions::view::SideLane],
    kept_ids: &std::collections::HashSet<&str>,
) -> Vec<kb_core::sessions::view::SideLane> {
    lanes
        .iter()
        .filter(|l| l.turn_ids.iter().any(|id| kept_ids.contains(id.as_str())))
        .cloned()
        .collect()
}

#[cfg(test)]
mod view_fields_and_turn_window_tests {
    use super::{parse_turn_window, ViewFields};

    #[test]
    fn view_fields_parse_csv_subset_and_ignores_unknown() {
        let f = ViewFields::parse("header,turns,not-a-field,stats");
        assert!(f.header);
        assert!(f.turns);
        assert!(!f.outline);
        assert!(!f.tasks);
        assert!(!f.subagents);
        assert!(!f.minimap);
        assert!(f.stats);
    }

    #[test]
    fn view_fields_parse_empty_is_all_false() {
        let f = ViewFields::parse("");
        assert!(!f.header && !f.turns && !f.outline && !f.tasks);
        assert!(!f.subagents && !f.minimap && !f.stats);
    }

    #[test]
    fn view_fields_all_enables_every_section() {
        let f = ViewFields::all();
        assert!(f.header && f.turns && f.outline && f.tasks);
        assert!(f.subagents && f.minimap && f.stats);
    }

    #[test]
    fn parse_turn_window_accepts_single_and_range() {
        assert_eq!(parse_turn_window("5"), Some((5, 5)));
        assert_eq!(parse_turn_window("5..7"), Some((5, 7)));
        assert_eq!(parse_turn_window("7..5"), Some((5, 7)));
        assert_eq!(parse_turn_window(""), None);
        assert_eq!(parse_turn_window("x..y"), None);
        assert_eq!(parse_turn_window("5.."), None);
    }
}

/// PF-R1 — the `?turns=` grammar + the pure turn/side-lane windowing it
/// drives. Mirrors `replay_tests`' convention of unit-testing the exact
/// pure functions the handler calls, with hand-built fixtures, rather than
/// standing up a daemon (no existing `/view`/`/replay` test in this file
/// does the latter).
#[cfg(test)]
mod turns_spec_tests {
    use super::{
        parse_turns_spec, window_side_lanes, window_turns_by_spec, TurnsSpec,
        VIEW_DEFAULT_TAIL_TURNS,
    };
    use kb_core::sessions::view::{Role, SideLane, Turn};

    fn turn(ordinal: u32) -> Turn {
        Turn {
            id: format!("t-{ordinal:012x}"),
            ordinal,
            role: Role::Human,
            ts: None,
            sidechain: false,
            agent_id: None,
            items: Vec::new(),
            raw_lines: Vec::new(),
        }
    }

    #[test]
    fn absent_is_the_default_tail_window() {
        assert_eq!(
            parse_turns_spec(None),
            TurnsSpec::Tail(VIEW_DEFAULT_TAIL_TURNS)
        );
        assert_eq!(
            parse_turns_spec(Some("")),
            TurnsSpec::Tail(VIEW_DEFAULT_TAIL_TURNS)
        );
    }

    #[test]
    fn all_is_the_escape_hatch() {
        assert_eq!(parse_turns_spec(Some("all")), TurnsSpec::All);
    }

    #[test]
    fn bare_int_is_a_tail_window_not_an_ordinal() {
        // PF-R1 redefines the pre-existing bare-number ORDINAL reading
        // (`parse_turn_window("5") == Some((5, 5))`) at this query-param
        // layer: nothing on the wire ever sent a bare `?turns=` number
        // (grepped web/src, kb-cli, tests/e2e — zero hits), so this is a
        // safe, deliberate reinterpretation.
        assert_eq!(parse_turns_spec(Some("5")), TurnsSpec::Tail(5));
        assert_eq!(parse_turns_spec(Some("0")), TurnsSpec::Tail(0));
    }

    #[test]
    fn ordinal_range_is_unchanged() {
        assert_eq!(parse_turns_spec(Some("5..7")), TurnsSpec::Ordinal(5, 7));
        assert_eq!(parse_turns_spec(Some("7..5")), TurnsSpec::Ordinal(5, 7));
    }

    #[test]
    fn malformed_degrades_to_the_default_window_never_to_all() {
        assert_eq!(
            parse_turns_spec(Some("x..y")),
            TurnsSpec::Tail(VIEW_DEFAULT_TAIL_TURNS)
        );
        assert_eq!(
            parse_turns_spec(Some("5..")),
            TurnsSpec::Tail(VIEW_DEFAULT_TAIL_TURNS)
        );
    }

    #[test]
    fn window_all_returns_everything() {
        let turns: Vec<_> = (1..=5).map(turn).collect();
        let kept = window_turns_by_spec(&turns, TurnsSpec::All);
        assert_eq!(kept.len(), 5, "?turns=all must match the old full response");
    }

    #[test]
    fn window_tail_keeps_only_the_last_n() {
        let turns: Vec<_> = (1..=10).map(turn).collect();
        let kept = window_turns_by_spec(&turns, TurnsSpec::Tail(3));
        assert_eq!(
            kept.iter().map(|t| t.ordinal).collect::<Vec<_>>(),
            vec![8, 9, 10],
            "a tail window keeps the LAST n turns, not the first"
        );
    }

    #[test]
    fn window_tail_larger_than_transcript_returns_everything() {
        let turns: Vec<_> = (1..=3).map(turn).collect();
        let kept = window_turns_by_spec(&turns, TurnsSpec::Tail(50));
        assert_eq!(
            kept.len(),
            3,
            "a window bigger than the session is a no-op, not a panic"
        );
    }

    #[test]
    fn window_tail_zero_returns_nothing() {
        let turns: Vec<_> = (1..=3).map(turn).collect();
        let kept = window_turns_by_spec(&turns, TurnsSpec::Tail(0));
        assert!(kept.is_empty());
    }

    #[test]
    fn window_ordinal_range_is_inclusive_both_ends() {
        let turns: Vec<_> = (1..=10).map(turn).collect();
        let kept = window_turns_by_spec(&turns, TurnsSpec::Ordinal(4, 6));
        assert_eq!(
            kept.iter().map(|t| t.ordinal).collect::<Vec<_>>(),
            vec![4, 5, 6]
        );
    }

    #[test]
    fn side_lanes_keep_only_lanes_touching_a_kept_turn() {
        let lanes = vec![
            SideLane {
                agent: None,
                turn_ids: vec!["t-a".to_string()],
                item_count: 1,
            },
            SideLane {
                agent: None,
                turn_ids: vec!["t-b".to_string(), "t-c".to_string()],
                item_count: 2,
            },
        ];
        let kept_ids: std::collections::HashSet<&str> = ["t-c"].into_iter().collect();
        let kept = window_side_lanes(&lanes, &kept_ids);
        assert_eq!(kept.len(), 1);
        assert_eq!(kept[0].turn_ids, vec!["t-b".to_string(), "t-c".to_string()]);
    }
}

#[cfg_attr(feature = "ts-export", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Serialize)]
pub struct SessionViewResponse {
    /// Always `session-view/1` — a consumer may refuse a shape it doesn't know.
    pub grammar: String,
    pub session_id: String,
    pub kb: String,
    pub artifact_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts-export", ts(optional))]
    pub header: Option<kb_core::sessions::view::ViewHeader>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts-export", ts(optional))]
    pub turns: Option<Vec<kb_core::sessions::view::Turn>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts-export", ts(optional))]
    pub side_lanes: Option<Vec<kb_core::sessions::view::SideLane>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts-export", ts(optional))]
    pub outline: Option<Vec<kb_core::sessions::view::OutlineRow>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts-export", ts(optional))]
    pub tasks: Option<Vec<kb_core::sessions::view::TaskBoardRow>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts-export", ts(optional))]
    pub subagents: Option<Vec<kb_core::sessions::view::SubagentSummary>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts-export", ts(optional))]
    pub minimap: Option<Vec<kb_core::sessions::view::MinimapPoint>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts-export", ts(optional))]
    pub stats: Option<kb_core::sessions::view::ViewStats>,
    /// PF-R1 — how many turns the FULL capture has, independent of
    /// whatever `?turns=` this response actually windowed to (mirrors
    /// `SessionReplayResponse::total_beats`).
    pub turns_total: usize,
    /// PF-R1 — how many turns THIS response's `?turns=` spec kept, before
    /// `?fields=` decides whether `turns`/`side_lanes` are even
    /// serialized. `turns_returned == turns_total` whenever `?turns=all`
    /// was asked for, or the default window covers the whole (short)
    /// session — so a caller can tell "was I windowed?" from these two
    /// ints alone, without diffing arrays.
    pub turns_returned: usize,
    /// Whether any redaction layer ran (mirrors `x-kb-session-scrubbed`).
    pub scrubbed: bool,
    /// How many redactions the scrub pass applied (mirrors `x-kb-redactions`).
    pub redactions: u32,
}

/// `GET /api/sessions/{session_id}/view?fields=&turns=&scrub=` — the
/// `session-view/1` interpreted IR (R1: ONE engine, three presenters — this
/// is the wire presenter). Newest-capture-scoped (`locate_capture`, #11),
/// scrub-FIRST then interpret (identical ordering to `/replay`, invariant
/// #4), LRU-cached on `(kb, artifact_id, mtime_unix, scrub-posture)` — the
/// FULL view is what's cached; `?fields=`/`?turns=` are response-time
/// projections over the cache hit, exactly like `/replay`'s `?artifact=`/
/// `?limit=`, so two different projections of one capture share one build.
/// PF-R1: a no-projection GET is no longer the whole transcript — absent
/// `?turns=` now defaults to the last `VIEW_DEFAULT_TAIL_TURNS` turns; a
/// caller that needs everything must ask with `?turns=all` (see
/// [`ViewQuery`]'s doc comment for the full grammar). `turns_total`/
/// `turns_returned` are additive wire fields so a consumer can always tell
/// it was windowed.
pub async fn view(
    State(state): State<Arc<KbHandles>>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Path(session_id): Path<String>,
    Query(q): Query<ViewQuery>,
) -> Response {
    let Some((kb, artifact_id, abs, mtime_unix)) = locate_capture(&state, &session_id).await else {
        return error_to_problem_json(&kb_core::Error::NotFound(format!("session {session_id}")))
            .into_response();
    };

    let mut scrub = parse_scrub_layers(&q.scrub);
    if crate::scrub::looks_non_loopback(Some(peer.ip()), &headers, &state.origin.trusted_proxies) {
        scrub.secrets = true;
    }
    let tag = scrub_tag(&scrub);

    let view: Arc<kb_core::sessions::view::SessionView>;
    let scrubbed;
    let redactions;
    if let Some(hit) = state.view_cache.get(&kb, &artifact_id, mtime_unix, &tag) {
        view = hit;
        // A cache hit's posture is exactly `tag` (the cache never serves a
        // different posture, invariant #4) — but the ORIGINAL scrub report
        // (whether anything actually matched) isn't itself cached, only the
        // built IR. Re-derive honestly: any layer requested ⇒ report as
        // scrubbed (matches `/replay`'s `report.total > 0` semantics closely
        // enough for a cached hit — the precise count is a build-time-only
        // artifact and not re-computed here to keep the cache cheap).
        scrubbed = scrub.any();
        redactions = 0;
    } else {
        let read = tokio::task::spawn_blocking(move || std::fs::read_to_string(&abs)).await;
        let html = match read {
            Ok(Ok(s)) => s,
            _ => {
                return error_to_problem_json(&kb_core::Error::NotFound(format!(
                    "capture unreadable for session {session_id}"
                )))
                .into_response()
            }
        };
        let Some(jsonl) = kb_core::sessions::recover_jsonl_from_capture(&html) else {
            return error_to_problem_json(&kb_core::Error::NotFound(format!(
                "no transcript in capture for session {session_id}"
            )))
            .into_response();
        };
        let built = tokio::task::spawn_blocking(move || {
            let (text, report) = if scrub.any() {
                kb_core::session_scrub::scrub_transcript(&jsonl, &scrub)
            } else {
                (jsonl, kb_core::session_scrub::ScrubReport::default())
            };
            let tail = kb_core::sessions::view::TailBlocks::from_html(&html);
            let built_view = kb_core::sessions::view::session_view(
                &text,
                &tail,
                &kb_core::sessions::view::ViewOptions::default(),
            );
            (built_view, report)
        })
        .await;
        let (built_view, report) = match built {
            Ok(v) => v,
            Err(e) => {
                tracing::warn!(session_id = %session_id, error = %e, "view build joined with error");
                return error_to_problem_json(&kb_core::Error::Internal(anyhow::anyhow!(
                    "view build failed: {e}"
                )))
                .into_response();
            }
        };
        scrubbed = report.total > 0;
        redactions = report.total;
        view = Arc::new(built_view);
        state
            .view_cache
            .put(&kb, &artifact_id, mtime_unix, &tag, view.clone());
    }

    let fields = ViewFields::from_query(&q.fields);
    // PF-R1: `turns_total`/`turns_returned` are computed unconditionally
    // (cheap — a slice/filter over the already-in-memory IR, no I/O, no
    // cache implications) since they're additive top-level metadata, not
    // gated by `?fields=turns`.
    let turns_spec = parse_turns_spec(q.turns.as_deref());
    let turns_total = view.turns.len();
    let kept_turns = window_turns_by_spec(&view.turns, turns_spec);
    let turns_returned = kept_turns.len();
    let kept_turn_ids: std::collections::HashSet<&str> =
        kept_turns.iter().map(|t| t.id.as_str()).collect();

    let body = SessionViewResponse {
        grammar: view.grammar.clone(),
        session_id: session_id.clone(),
        kb,
        artifact_id,
        header: fields.header.then(|| view.header.clone()),
        turns: fields
            .turns
            .then(|| kept_turns.iter().map(|t| (*t).clone()).collect()),
        side_lanes: fields
            .turns
            .then(|| window_side_lanes(&view.side_lanes, &kept_turn_ids)),
        outline: fields.outline.then(|| view.outline.clone()),
        tasks: fields.tasks.then(|| view.tasks_final.clone()),
        subagents: fields.subagents.then(|| view.subagents.clone()),
        minimap: fields.minimap.then(|| view.minimap.clone()),
        stats: fields.stats.then(|| view.stats.clone()),
        turns_total,
        turns_returned,
        scrubbed,
        redactions,
    };
    let mut resp = Json(body).into_response();
    let h = resp.headers_mut();
    h.insert(
        "x-kb-session-scrubbed",
        HeaderValue::from_static(if scrubbed { "1" } else { "0" }),
    );
    if let Ok(v) = HeaderValue::from_str(&redactions.to_string()) {
        h.insert("x-kb-redactions", v);
    }
    resp
}

/// `GET /api/sessions/{session_id}/raw?scrub=` — the decoded JSONL transcript,
/// `text/plain`, byte-identical to what `claude -r`/`kb sessions read --raw`
/// would see. The least-mediated surface kb serves (no interpretation layer
/// over it at all), so a non-loopback client's floor is STRICTER than
/// `/export`/`/replay`/`/view`'s secrets-only force: entropy is forced on
/// too (mirrors the export-route precedent's shape, `routes/sessions.rs`
/// `export`, one layer stronger). Newest-capture-scoped (#11).
/// `Cache-Control: no-store` — this is the rawest possible read of
/// (potentially still-sensitive) transcript text; never cached client-side.
pub async fn raw(
    State(state): State<Arc<KbHandles>>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Path(session_id): Path<String>,
    Query(q): Query<ExportQuery>,
) -> Response {
    let Some((_kb, _artifact_id, abs, _mtime_unix)) = locate_capture(&state, &session_id).await
    else {
        return error_to_problem_json(&kb_core::Error::NotFound(format!("session {session_id}")))
            .into_response();
    };
    let read = tokio::task::spawn_blocking(move || std::fs::read_to_string(&abs)).await;
    let html = match read {
        Ok(Ok(s)) => s,
        _ => {
            return error_to_problem_json(&kb_core::Error::NotFound(format!(
                "capture unreadable for session {session_id}"
            )))
            .into_response()
        }
    };
    let Some(jsonl) = kb_core::sessions::recover_jsonl_from_capture(&html) else {
        return error_to_problem_json(&kb_core::Error::NotFound(format!(
            "no transcript in capture for session {session_id}"
        )))
        .into_response();
    };

    let mut scrub = parse_scrub_layers(&q.scrub);
    if crate::scrub::looks_non_loopback(Some(peer.ip()), &headers, &state.origin.trusted_proxies) {
        scrub.secrets = true;
        scrub.entropy = true;
    }
    let built = tokio::task::spawn_blocking(move || {
        if scrub.any() {
            kb_core::session_scrub::scrub_transcript(&jsonl, &scrub)
        } else {
            (jsonl, kb_core::session_scrub::ScrubReport::default())
        }
    })
    .await;
    let (text, report) = match built {
        Ok(v) => v,
        Err(e) => {
            tracing::warn!(session_id = %session_id, error = %e, "raw scrub joined with error");
            return error_to_problem_json(&kb_core::Error::Internal(anyhow::anyhow!(
                "raw scrub failed: {e}"
            )))
            .into_response();
        }
    };

    let mut resp = (StatusCode::OK, text).into_response();
    let h = resp.headers_mut();
    h.insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("text/plain; charset=utf-8"),
    );
    h.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    h.insert(
        "x-kb-session-scrubbed",
        HeaderValue::from_static(if report.total > 0 { "1" } else { "0" }),
    );
    if let Ok(v) = HeaderValue::from_str(&report.total.to_string()) {
        h.insert("x-kb-redactions", v);
    }
    resp
}

// --- W7 (sessions-rethink R15/LF-1/LF-3b) — live-follow ---------------------

/// A 403 that never leaks anything about WHY (LF-5's flat rule: refuse
/// non-loopback with 403, always `no-store` — unlike `/export`/`/view`'s
/// scrub-floor-and-serve posture, there is no redaction layer that makes
/// serving a live, mid-flight, unscrubbed transcript safe to a non-loopback
/// caller, so this is a hard refusal rather than a forced floor).
fn loopback_only_forbidden() -> Response {
    let mut resp = error_to_problem_json(&kb_core::Error::Forbidden(
        "the live-session lane (presence/live) is loopback-only in v1".to_string(),
    ));
    resp.headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    resp
}

#[cfg_attr(feature = "ts-export", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Clone, Serialize)]
pub struct PresenceEntry {
    pub session_id: String,
    pub mtime_unix: i64,
    pub bytes: u64,
    pub project_slug: String,
}

#[cfg_attr(feature = "ts-export", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Serialize)]
pub struct PresenceResponse {
    /// `false` when `[sessions] live_transcripts_dir` is unconfigured — the
    /// stable, cheap answer every prod/phone deployment gets (LF-1).
    pub enabled: bool,
    pub live: Vec<PresenceEntry>,
}

/// `GET /api/sessions/presence` — LF-1's Tier-1 stat-only presence probe:
/// which live transcripts (under `[sessions] live_transcripts_dir`) have
/// been written to within `live_window_secs`. Static route, registered
/// BEFORE `/{session_id}` (router.rs ordering rule) — this can never be
/// shadowed by (or shadow) a real session id.
///
/// **Loopback-only HARD (LF-5)**: `project_slug` discloses filesystem path
/// structure, and unlike `/export`/`/view`/`/raw` there is no scrub layer
/// that makes this safe to serve non-loopback — a non-loopback request
/// 403s even with a valid bearer token (stricter than `auth_bearer`'s own
/// posture; invariant #4's fail-closed ethos extended to this lane).
///
/// Cheap by construction (LF-1 lesson #1, design archaeology from a
/// retired internal cockpit prototype): a bounded `readdir` walk, `stat` only — no
/// transcript file is ever opened here.
pub async fn presence(
    State(state): State<Arc<KbHandles>>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
) -> Response {
    // Loopback gate FIRST (LF-5) — before any cache read or config await.
    if !crate::middleware::is_loopback_origin(
        Some(peer.ip()),
        &headers,
        &state.origin.trusted_proxies,
    ) {
        return loopback_only_forbidden();
    }

    let sessions_cfg = state.config.read().await.sessions.clone();
    let Some(dir) = sessions_cfg.resolved_live_dir() else {
        return no_store_json(PresenceResponse {
            enabled: false,
            live: vec![],
        });
    };
    let window_secs = sessions_cfg.live_window_secs() as i64;

    // ~1s TTL in-process cache per live-dir path (PSRV-3). Guard is dropped
    // before the await (#15); a miss pays one readdir walk.
    if let Some(hit) = presence_cache_get(&dir, window_secs) {
        return no_store_json(PresenceResponse {
            enabled: true,
            live: hit,
        });
    }

    let dir_for_scan = dir.clone();
    let live = tokio::task::spawn_blocking(move || scan_live_presence(&dir_for_scan, window_secs))
        .await
        .unwrap_or_else(|e| {
            tracing::warn!(error = %e, "presence scan join error");
            Vec::new()
        });
    presence_cache_put(dir, window_secs, live.clone());
    no_store_json(PresenceResponse {
        enabled: true,
        live,
    })
}

/// ~1s TTL snapshot of the last successful presence scan, keyed on
/// `(live_dir, window_secs)`. Process-local; never held across `.await`.
const PRESENCE_CACHE_TTL: std::time::Duration = std::time::Duration::from_secs(1);

struct PresenceCacheEntry {
    dir: std::path::PathBuf,
    window_secs: i64,
    at: std::time::Instant,
    live: Vec<PresenceEntry>,
}

fn presence_cache() -> &'static std::sync::Mutex<Option<PresenceCacheEntry>> {
    static CACHE: std::sync::OnceLock<std::sync::Mutex<Option<PresenceCacheEntry>>> =
        std::sync::OnceLock::new();
    CACHE.get_or_init(|| std::sync::Mutex::new(None))
}

fn presence_cache_get(dir: &std::path::Path, window_secs: i64) -> Option<Vec<PresenceEntry>> {
    let guard = presence_cache().lock().unwrap_or_else(|e| e.into_inner());
    let entry = guard.as_ref()?;
    if entry.dir != dir || entry.window_secs != window_secs {
        return None;
    }
    if entry.at.elapsed() > PRESENCE_CACHE_TTL {
        return None;
    }
    Some(entry.live.clone())
}

fn presence_cache_put(dir: std::path::PathBuf, window_secs: i64, live: Vec<PresenceEntry>) {
    let mut guard = presence_cache().lock().unwrap_or_else(|e| e.into_inner());
    *guard = Some(PresenceCacheEntry {
        dir,
        window_secs,
        at: std::time::Instant::now(),
        live,
    });
}

fn no_store_json<T: Serialize>(body: T) -> Response {
    let mut resp = Json(body).into_response();
    resp.headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    resp
}

/// Bounded `readdir` walk: `<dir>/*/*.jsonl`, depth 1, capped at
/// [`PRESENCE_SCAN_CAP`] entries total (LF-1). Blocking — the caller
/// `spawn_blocking`s this so a slow/huge directory never stalls the async
/// runtime.
const PRESENCE_SCAN_CAP: usize = 4096;

// `scanned` accumulates across BOTH loop levels (a global cap over every
// project subdirectory, not a per-directory `enumerate()`), so clippy's
// "use enumerate" suggestion doesn't apply here — it would reset the count
// at each project boundary instead of capping the whole walk.
#[allow(clippy::explicit_counter_loop)]
fn scan_live_presence(dir: &std::path::Path, window_secs: i64) -> Vec<PresenceEntry> {
    let mut out = Vec::new();
    let mut scanned = 0usize;
    let now = chrono::Utc::now().timestamp();
    let Ok(project_dirs) = std::fs::read_dir(dir) else {
        return out;
    };
    'outer: for proj in project_dirs.flatten() {
        let proj_path = proj.path();
        if !proj_path.is_dir() {
            continue;
        }
        let slug = proj.file_name().to_string_lossy().into_owned();
        let Ok(files) = std::fs::read_dir(&proj_path) else {
            continue;
        };
        for f in files.flatten() {
            if scanned >= PRESENCE_SCAN_CAP {
                break 'outer;
            }
            scanned += 1;
            let p = f.path();
            if p.extension().and_then(|e| e.to_str()) != Some("jsonl") {
                continue;
            }
            let Ok(meta) = f.metadata() else { continue };
            let Ok(modified) = meta.modified() else {
                continue;
            };
            let mtime_unix = modified
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_secs() as i64)
                .unwrap_or(0);
            if now - mtime_unix > window_secs {
                continue;
            }
            let Some(sid) = p.file_stem().and_then(|s| s.to_str()) else {
                continue;
            };
            out.push(PresenceEntry {
                session_id: sid.to_string(),
                mtime_unix,
                bytes: meta.len(),
                project_slug: slug.clone(),
            });
        }
    }
    out
}

/// Query for `GET /api/sessions/{sid}/live`.
#[derive(Debug, Default, Deserialize)]
pub struct LiveQuery {
    /// Byte offset to tail from. Absent (or a value with no matching cached
    /// carry) triggers a stateless bootstrap over the last 2 MiB — see
    /// `kb_core::sessions::tail::read_bootstrap_window`.
    from: Option<u64>,
    /// `raw=1` (or `true`/`on` — [`wants_raw_flag`]'s tolerant grammar,
    /// mirroring `routes::artifact::wants_raw`) returns decoded JSONL lines
    /// verbatim instead of interpreted `session-view/1` events — no carry,
    /// no bootstrap, no join state. A bare `bool` field would reject
    /// `raw=1` outright (`serde`'s bool deserializer only accepts the
    /// literal strings `"true"`/`"false"`), so this is a `String` parsed
    /// leniently instead.
    raw: Option<String>,
}

/// Lenient boolean query-flag parse — `"1"`, `"true"`, or `"on"` (mirrors
/// `routes::artifact::wants_raw`'s grammar). Anything else (including
/// absent) is `false`.
fn wants_raw_flag(v: &Option<String>) -> bool {
    matches!(v.as_deref(), Some("1") | Some("true") | Some("on"))
}

#[cfg_attr(feature = "ts-export", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Serialize)]
pub struct LiveDeltaResponse {
    pub sid: String,
    pub from: u64,
    pub next_from: u64,
    pub size: u64,
    /// `mtime(file) < live_window_secs` — a writer is active right now.
    pub live: bool,
    /// Advisory: a capture newer than the live file's last write exists AND
    /// `live` is false. The client's REAL stop signal is the
    /// `session.captured` SSE event, never this field.
    pub ended: bool,
    pub truncated_restart: bool,
    /// Lines that could never be interpreted at all this delta: unparseable
    /// complete JSONL lines (`ViewCarry::stats().unparsed`) plus an
    /// oversized unterminated fragment the tailer had to drop, if any.
    /// Always `0` in `raw` mode (nothing is parsed).
    pub parse_failures: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts-export", ts(optional))]
    pub events: Option<Vec<kb_core::sessions::view::ViewEvent>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts-export", ts(optional))]
    pub raw_lines: Option<Vec<String>>,
}

/// `GET /api/sessions/{sid}/live?from=&raw=` — LF-3b: a loopback-only,
/// request-response DELTA over the live JSONL transcript (`[sessions]
/// live_transcripts_dir`). No standing stream (#24) — the SPA follow-mode
/// panel and `kb sessions read --follow` both POLL this every few seconds
/// while actively following; each response is a complete-lines-only
/// consistent window the client loops over (bump `from` to `next_from`)
/// until `next_from == size`.
///
/// **Loopback-only HARD (LF-5)** — same posture as [`presence`], stricter
/// than `/export`/`/view`/`/raw`: a live transcript is unscrubbed,
/// mid-flight content the operator hasn't seen yet, so this refuses rather
/// than forces a redaction floor.
///
/// Flow: gate → 404 if Tier-1 isn't configured → resolve `sid` → path via
/// the SHARED [`kb_core::sessions::tail::resolve_live_transcript`] (the
/// SAME resolver `kb sessions read --live`/`--follow` calls, LF-4) → serve
/// either the interpreted delta (via the server-side
/// [`crate::live_tail_cache::LiveTailCache`], falling back to a stateless
/// bootstrap on any miss — the cache is NEVER load-bearing) or, under
/// `raw=1`, the decoded lines verbatim with no interpretation at all.
pub async fn live(
    State(state): State<Arc<KbHandles>>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Path(session_id): Path<String>,
    Query(q): Query<LiveQuery>,
) -> Response {
    if !crate::middleware::is_loopback_origin(
        Some(peer.ip()),
        &headers,
        &state.origin.trusted_proxies,
    ) {
        return loopback_only_forbidden();
    }

    let sessions_cfg = state.config.read().await.sessions.clone();
    if sessions_cfg.resolved_live_dir().is_none() {
        let mut resp = no_store_json(serde_json::json!({"enabled": false}));
        *resp.status_mut() = StatusCode::NOT_FOUND;
        return resp;
    }

    // Best-effort: the newest capture's cwd (if any) sharpens the resolver's
    // first rung (`claude_project_slug`); a never-captured sid falls through
    // to its bounded glob fallback. `ended_at` feeds the advisory `ended`
    // field. ONE fan-out for both (not two `sessions_get` sweeps).
    let (newest_cwd, captured_ended_at) = locate_capture_cwd_and_ended(&state, &session_id).await;

    let sid_for_resolve = session_id.clone();
    let resolved = tokio::task::spawn_blocking(move || {
        kb_core::sessions::tail::resolve_live_transcript(
            &sid_for_resolve,
            &sessions_cfg,
            newest_cwd.as_deref(),
        )
    })
    .await
    .ok()
    .flatten();
    let Some(path) = resolved else {
        let mut resp = error_to_problem_json(&kb_core::Error::NotFound(format!(
            "no live transcript for session {session_id}"
        )));
        resp.headers_mut()
            .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
        return resp;
    };

    let window_secs = { state.config.read().await.sessions.live_window_secs() as i64 };
    let from = q.from.unwrap_or(0);
    let raw_mode = wants_raw_flag(&q.raw);
    let sid = session_id.clone();
    let cache = state.live_tail_cache.clone();

    let built = tokio::task::spawn_blocking(move || -> std::io::Result<LiveDeltaResponse> {
        build_live_delta(
            &sid,
            &path,
            from,
            raw_mode,
            window_secs,
            captured_ended_at,
            &cache,
        )
    })
    .await;

    match built {
        Ok(Ok(body)) => no_store_json(body),
        Ok(Err(e)) => {
            tracing::warn!(session_id = %session_id, error = %e, "live delta read failed");
            // NotFound → 404 as before; every other IO kind → 500 with the
            // kind in the message (mirrors capture's PermissionDenied→Conflict
            // precedent — classify rather than collapse all IO to 404).
            let mut resp = if e.kind() == std::io::ErrorKind::NotFound {
                error_to_problem_json(&kb_core::Error::NotFound(format!(
                    "live transcript unreadable for session {session_id}"
                )))
            } else {
                error_to_problem_json(&kb_core::Error::Internal(anyhow::anyhow!(
                    "live transcript IO error for session {session_id}: {:?}",
                    e.kind()
                )))
            };
            resp.headers_mut()
                .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
            resp
        }
        Err(e) => {
            tracing::warn!(session_id = %session_id, error = %e, "live delta build joined with error");
            let mut resp = error_to_problem_json(&kb_core::Error::Internal(anyhow::anyhow!(
                "live delta build failed: {e}"
            )));
            resp.headers_mut()
                .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
            resp
        }
    }
}

/// The blocking half of [`live`]: all filesystem IO + the (de)serialization-
/// free interpretation pass. Pure enough to unit-test directly against a
/// scratch file (see the `tests` module).
#[allow(clippy::too_many_arguments)]
fn build_live_delta(
    sid: &str,
    path: &std::path::Path,
    from: u64,
    raw_mode: bool,
    window_secs: i64,
    captured_ended_at: Option<i64>,
    cache: &crate::live_tail_cache::LiveTailCache,
) -> std::io::Result<LiveDeltaResponse> {
    let meta = std::fs::metadata(path)?;
    let mtime_unix = meta
        .modified()
        .ok()
        .and_then(|m| m.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    let now = chrono::Utc::now().timestamp();
    let live = now - mtime_unix <= window_secs;
    let ended = !live && captured_ended_at.is_some_and(|cap| cap > mtime_unix);

    if raw_mode {
        let reader = kb_core::sessions::tail::TailReader::new(path.to_path_buf(), from);
        let delta = reader.read_delta(kb_core::sessions::tail::LIVE_DELTA_MAX_BYTES)?;
        let raw_lines: Vec<String> = delta
            .complete_lines
            .lines()
            .map(|l| l.to_string())
            .collect();
        return Ok(LiveDeltaResponse {
            sid: sid.to_string(),
            from,
            next_from: delta.next_offset,
            size: delta.size,
            live,
            ended,
            truncated_restart: delta.truncated_restart,
            parse_failures: u32::from(delta.fragment_dropped),
            events: None,
            raw_lines: Some(raw_lines),
        });
    }

    let inode = crate::live_tail_cache::file_inode(&meta);

    // Try the incremental path: a cache hit at EXACTLY `from` lets us tail
    // just the new bytes and feed them into the existing carry.
    if let Some(carry) = cache.get(sid, inode, from) {
        let reader = kb_core::sessions::tail::TailReader::new(path.to_path_buf(), from);
        let delta = reader.read_delta(kb_core::sessions::tail::LIVE_DELTA_MAX_BYTES)?;
        if !delta.truncated_restart {
            let (events, new_carry) =
                kb_core::sessions::view::view_append(carry, &delta.complete_lines);
            let parse_failures = new_carry.stats().unparsed + u32::from(delta.fragment_dropped);
            cache.put(sid, inode, delta.next_offset, new_carry);
            return Ok(LiveDeltaResponse {
                sid: sid.to_string(),
                from,
                next_from: delta.next_offset,
                size: delta.size,
                live,
                ended,
                truncated_restart: false,
                parse_failures,
                events: Some(events),
                raw_lines: None,
            });
        }
        // Fall through to the stateless bootstrap below — the file was
        // truncated/rotated/rewritten out from under the cached carry.
    }

    // Stateless bootstrap fallback (LF-4: "always correct" — a cold start,
    // a `from` with no matching carry, or the truncation case above all
    // land here). Ignores the client's `from` for INTERPRETATION purposes
    // (the live view is a tail window, not the full document); `next_from`
    // reflects the real resumption point regardless of what was requested.
    let (window, resume_offset) = kb_core::sessions::tail::read_bootstrap_window(path)?;
    let carry = kb_core::sessions::view::view_bootstrap(&window);
    let parse_failures = carry.stats().unparsed;
    let size = std::fs::metadata(path)?.len();
    cache.put(sid, inode, resume_offset, carry);
    Ok(LiveDeltaResponse {
        sid: sid.to_string(),
        from,
        next_from: resume_offset,
        size,
        live,
        ended,
        truncated_restart: from > resume_offset,
        parse_failures,
        events: Some(Vec::new()),
        raw_lines: None,
    })
}

/// Same fan-out shape as [`locate_capture`], projected to just `(cwd,
/// ended_at)` — the `/live` route's two best-effort inputs (the resolver's
/// cwd→slug rung, and the advisory `ended` field) — in ONE `sessions_get`
/// sweep rather than two. A separate query rather than reusing
/// `locate_capture`'s tuple because that type is already named + used
/// elsewhere (`LocatedSession`-shaped call sites) without a `cwd` field.
async fn locate_capture_cwd_and_ended(
    state: &Arc<KbHandles>,
    session_id: &str,
) -> (Option<String>, Option<i64>) {
    type CwdEnded = (Option<String>, i64);
    let mut futs: Vec<super::CorpusFut<'_, Option<CwdEnded>>> = Vec::new();
    for (kb_name, ctx) in state.kbs.iter() {
        futs.push(Box::pin(async move {
            match ctx.storage.sessions_get(session_id.to_string()).await {
                Ok(Some(row)) => Some((row.cwd, row.ended_at)),
                Ok(None) => None,
                Err(e) => {
                    tracing::warn!(kb = %kb_name, session_id = %session_id, error = %e, "sessions_get failed in locate_capture_cwd_and_ended");
                    None
                }
            }
        }));
    }
    // PF-R1 — the operator-configurable `[server] fanout_cap` (default 8,
    // byte-identical to the old hardcoded `super::FANOUT_CAP`).
    match super::buffered_join(futs, state.fanout_cap)
        .await
        .into_iter()
        .flatten()
        .next()
    {
        Some((cwd, ended_at)) => (cwd, Some(ended_at)),
        None => (None, None),
    }
}

// --- LSC-2 (docs/research/kb-live-sessions-cockpit-2026-08.html §6
// "Shaping the server") — the live-sessions cockpit's DAEMON side: beat
// intake into `state.live_registry` (`crate::live_registry` — storage is
// NOTHING, LF-7) and the merged read layering it over a Tier-0 degraded
// view rebuilt from landed captures. Daemon-wide, NOT per-kb (a beat
// arrives before any capture exists, so there is no kb to attribute it to
// yet) — unlike every other route in this file, neither route takes a
// `{kb}` path segment. `auth_bearer`, NOT loopback-only (the router.rs
// `/api` tree already layers it) — the one deliberate departure from
// `presence`/`live` above; see [`live_status`]'s doc comment for why that's
// safe. ------------------------------------------------------------------

/// `POST /api/sessions/beat` request body — MUST match `plugins/kb-memory/
/// hooks/kb-beat.sh`'s JSON exactly (LSC-3, already shipping beats at this
/// route before it existed — they 404'd until this landed). Every field
/// beyond `session_id`/`harness`/`event`/`at` really is optional: the hook
/// omits `title` entirely (only the design's wire example carries it), and
/// this struct must be just as liberal — an UNDECLARED JSON key is silently
/// ignored by serde's default behaviour (no `deny_unknown_fields`, on
/// purpose: the hook and the daemon will version-skew across deploys,
/// design §6/"WHAT TO BUILD" B).
#[derive(Debug, Deserialize)]
pub struct BeatBody {
    #[serde(default)]
    pub v: Option<i64>,
    pub session_id: String,
    pub harness: String,
    pub event: String,
    pub at: String,
    pub host: Option<String>,
    pub pid: Option<i64>,
    pub cwd: Option<String>,
    pub model: Option<String>,
    #[serde(default)]
    pub lease_secs: Option<i64>,
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default)]
    pub last_line: Option<String>,
    #[serde(default)]
    pub detail: Option<BeatDetail>,
}

#[derive(Debug, Deserialize)]
pub struct BeatDetail {
    #[serde(default)]
    pub reason: Option<String>,
    /// Design §5's wire example carries `tool` alongside `reason`;
    /// kb-beat.sh (LSC-3) never sends it. Declared-but-unused rather than
    /// omitted so the wire shape is documented honestly (an actually
    /// undeclared field would be ignored anyway).
    #[serde(default)]
    pub tool: Option<String>,
}

/// kb-beat.sh's own default (`KB_BEAT_LEASE_SECS`, design §5's example
/// value) — applied when a beat omits `lease_secs` entirely.
const DEFAULT_LEASE_SECS: i64 = 900;

/// LSC-2 Tier-0 — per-kb scan bound for the "recent captures with no
/// registry entry" degraded layer (design §6). Generous relative to the
/// corpus's measured scale (design §4: 55 sessions total on this box) but
/// bounded so a huge sessions corpus can't turn this into an unbounded
/// per-kb table scan. `sessions_list` already applies `newest_capture_pred`
/// (invariant #11), so this is one collapsed row per `session_id`, never
/// one per capture.
const TIER0_SCAN_LIMIT: u32 = 500;

/// Map a beat's `event` to the [`Holder`] axis plus whether it's a
/// `blocked` beat — the ONLY interpretation the daemon does on intake
/// (design "WHAT TO BUILD" B): `start`/`prompt`/`tool`/`unblocked` →
/// `Agent`; `turn_end` → `Human`; `blocked` → `Agent` (+ the flag, so a
/// presenter can render a distinct waiting-lane reason without a new
/// `LiveState` variant); `end` → `Ended`. `None` for anything outside this
/// closed vocabulary — the caller 400s rather than guessing.
fn map_event(event: &str) -> Option<(Holder, bool)> {
    match event {
        "start" | "prompt" | "tool" | "unblocked" => Some((Holder::Agent, false)),
        "turn_end" => Some((Holder::Human, false)),
        "blocked" => Some((Holder::Agent, true)),
        "end" => Some((Holder::Ended, false)),
        _ => None,
    }
}

/// Parse the beat's `at` field: RFC 3339 (`kb-beat.sh` emits `date -u
/// +%Y-%m-%dT%H:%M:%SZ`), falling back to a bare unix-seconds integer
/// (forward-compat for an adapter that skips the ISO round-trip). `None` on
/// anything else — the caller 400s.
fn parse_beat_at(at: &str) -> Option<i64> {
    if let Ok(dt) = chrono::DateTime::parse_from_rfc3339(at) {
        return Some(dt.timestamp());
    }
    at.trim().parse::<i64>().ok()
}

/// Basename-of-cwd project label — mirrors `SessionOut::from_row`'s
/// `folder` derivation (the same "readable project name" convention used
/// throughout `/api/sessions/*`), so a beat-derived row's `project` field
/// reads identically to a captured session's.
fn derive_project(cwd: Option<&str>) -> Option<String> {
    cwd.map(|c| {
        c.trim_end_matches('/')
            .rsplit('/')
            .next()
            .unwrap_or(c)
            .to_string()
    })
}

/// Best-effort `resume` string. `claude -r <sid>` is the ONLY verified
/// convention (mirrors `classify_claude_transcript`'s exact format, LSC-1);
/// LSC-3 only wires Claude Code beats today (`hooks.json`), so every other
/// harness's resume UX is genuinely unknown at this phase — `<harness>
/// resume <sid>` is a readable placeholder, not a verified command.
fn resume_command(harness: &str, session_id: &str) -> String {
    if harness == "claude" {
        format!("claude -r {session_id}")
    } else {
        format!("{harness} resume {session_id}")
    }
}

/// `POST /api/sessions/beat` — LSC-2/3's push intake: map one lifecycle
/// EVENT (never a state — the daemon derives state, design §5's whole
/// discipline) into `state.live_registry`, then fire `session.state` iff
/// the derived state actually CHANGED (never once per beat, or a busy
/// fleet would flood every browser, invariant #24).
///
/// Cheap by construction: no storage-actor round trip, no disk IO — this
/// fires on every turn of every session (design §11's "hook latency" risk
/// list).
///
/// Liberal in what it accepts: an unknown-but-well-formed JSON key is
/// silently ignored (serde default, see [`BeatBody`]); an unknown `event`,
/// unknown `harness`, empty `session_id`, or unparseable `at` 400s with a
/// problem+json body. Never a panic, never a 500.
pub async fn beat(State(state): State<Arc<KbHandles>>, Json(body): Json<BeatBody>) -> Response {
    let Some((holder, blocked)) = map_event(&body.event) else {
        return error_to_problem_json(&kb_core::Error::BadRequest(format!(
            "unknown beat event {:?} (expected one of: start, prompt, tool, \
             turn_end, blocked, unblocked, end)",
            body.event
        )));
    };
    if !kb_core::sessions::HARNESSES.contains(&body.harness.as_str()) {
        return error_to_problem_json(&kb_core::Error::BadRequest(format!(
            "unknown beat harness {:?} (expected one of: {:?})",
            body.harness,
            kb_core::sessions::HARNESSES
        )));
    }
    if body.session_id.trim().is_empty() {
        return error_to_problem_json(&kb_core::Error::BadRequest(
            "beat session_id must not be empty".to_string(),
        ));
    }
    let Some(at_unix) = parse_beat_at(&body.at) else {
        return error_to_problem_json(&kb_core::Error::BadRequest(format!(
            "unparseable beat 'at' timestamp: {:?}",
            body.at
        )));
    };

    let now_unix = chrono::Utc::now().timestamp();
    let last_line = body.last_line.filter(|s| !s.trim().is_empty()).map(|s| {
        kb_core::sessions::truncate_chars_ellipsis(&s, kb_core::sessions::OUTCOME_WIRE_MAX_CHARS)
    });
    let detail_reason = body
        .detail
        .and_then(|d| d.reason)
        .filter(|s| !s.trim().is_empty());

    let row = crate::live_registry::LiveRow {
        session_id: body.session_id,
        harness: body.harness,
        holder,
        last_activity_unix: at_unix,
        lease_secs: body.lease_secs.unwrap_or(DEFAULT_LEASE_SECS).max(1),
        host: body.host,
        pid: body.pid,
        cwd: body.cwd,
        model: body.model,
        title: body.title,
        last_line,
        blocked,
        detail_reason,
        touched_unix: now_unix,
    };

    let outcome = state.live_registry.record_beat(row, now_unix);

    if outcome.previous_state != Some(outcome.state) {
        state.bus.emit(
            "session.state",
            serde_json::json!({
                "session_id": outcome.row.session_id,
                "harness": outcome.row.harness,
                "state": outcome.state,
                "holder": outcome.row.holder,
                "project": derive_project(outcome.row.cwd.as_deref()),
                "since_unix": outcome.row.last_activity_unix,
            }),
        );
    }

    Json(serde_json::json!({
        "ok": true,
        "session_id": outcome.row.session_id,
        "state": outcome.state,
        "holder": outcome.row.holder,
        "confidence": outcome.confidence,
    }))
    .into_response()
}

/// One row in `GET /api/sessions/live-status`'s response (design §5 "the
/// derived state" wire shape, reconciled to this route's actual field
/// names — see `kb_core::sessions::live::LiveSession`'s own doc comment for
/// why that pure/offline type doesn't try to match the wire verbatim
/// either). `kb`/`artifact_id` are populated ONLY for a Tier-0
/// capture-derived row (`source: "capture"`) — a registry-sourced row
/// (`source: "hook"`) carries neither: a beat arrives before any capture
/// exists to attribute it to, and the merge rule (design §6) is a registry
/// entry always WINS over a capture-derived row for the same `session_id`,
/// never an enrichment join between the two.
#[cfg_attr(feature = "ts-export", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Clone, Serialize)]
pub struct LiveStatusRow {
    pub session_id: String,
    pub harness: String,
    pub holder: Holder,
    pub state: LiveState,
    pub source: StateSource,
    pub confidence: kb_core::sessions::live::Confidence,
    pub since_unix: i64,
    pub since_secs: i64,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts-export", ts(optional))]
    pub project: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts-export", ts(optional))]
    pub cwd: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts-export", ts(optional))]
    pub model: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts-export", ts(optional))]
    pub title: Option<String>,
    pub resume: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts-export", ts(optional))]
    pub last_line: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts-export", ts(optional))]
    pub kb: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts-export", ts(optional))]
    pub artifact_id: Option<String>,
    /// [`map_event`]'s holder mapping keeps `blocked` a flag on
    /// `Holder::Agent` rather than a new `LiveState` variant — a presenter
    /// may render this + `detail_reason` as a distinct sub-label inside the
    /// working/stalled lane.
    pub blocked: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts-export", ts(optional))]
    pub detail_reason: Option<String>,
}

#[cfg_attr(feature = "ts-export", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Serialize)]
pub struct LiveStatusResponse {
    pub sessions: Vec<LiveStatusRow>,
}

/// Query for `GET /api/sessions/live-status`.
#[derive(Debug, Default, Deserialize)]
pub struct LiveStatusQuery {
    /// csv over the six-state wire vocabulary.
    pub state: Option<String>,
    /// csv over `kb_core::sessions::HARNESSES`.
    pub harness: Option<String>,
    /// Exact match (case-insensitive) against the derived `project` field.
    pub project: Option<String>,
    /// Caps EACH lane independently — see [`order_and_cap_live_status_rows`].
    pub limit: Option<usize>,
}

/// csv over the six-state wire vocabulary. Unknown tokens are DROPPED
/// (never a 400) — this filter's own rule, UNCHANGED by the OK1 amendment
/// to [`parse_harness_csv`] (harness now 400s on an unknown EXPLICIT
/// token; this state filter was not in scope for that hardening and keeps
/// its original forward-compat-lenient behaviour).
fn parse_live_state_csv(raw: &str) -> Vec<LiveState> {
    raw.split(',')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .filter_map(|s| match s {
            "working" => Some(LiveState::Working),
            "stalled" => Some(LiveState::Stalled),
            "waiting" => Some(LiveState::Waiting),
            "cold" => Some(LiveState::Cold),
            "finished" => Some(LiveState::Finished),
            "presumed_ended" => Some(LiveState::PresumedEnded),
            _ => None,
        })
        .collect()
}

/// Design §7/§8's per-lane ordering, applied server-side so `GET
/// /api/sessions/live-status`'s response is already correctly ordered
/// (and, when `limit` is `Some`, already correctly capped) without every
/// consumer re-implementing the three-way split: working+stalled sort
/// most-recently-active-first, waiting sorts longest-wait-first, and
/// cold+finished+presumed_ended sort newest-first — concatenated in that
/// lane order so a flat consumer sees a sensible default too. `limit`, when
/// present, caps EACH lane independently (mirrors `kb sessions status
/// --local`'s `group_lanes`, design §8 — a small `--limit` still shows a
/// balanced snapshot rather than letting a busy IN PROGRESS lane crowd
/// WAITING out entirely).
fn order_and_cap_live_status_rows(
    rows: Vec<LiveStatusRow>,
    limit: Option<usize>,
) -> Vec<LiveStatusRow> {
    let mut in_progress: Vec<LiveStatusRow> = Vec::new();
    let mut waiting: Vec<LiveStatusRow> = Vec::new();
    let mut finished: Vec<LiveStatusRow> = Vec::new();
    for r in rows {
        match r.state {
            LiveState::Working | LiveState::Stalled => in_progress.push(r),
            LiveState::Waiting => waiting.push(r),
            LiveState::Cold | LiveState::Finished | LiveState::PresumedEnded => finished.push(r),
        }
    }
    in_progress.sort_by_key(|r| r.since_secs);
    waiting.sort_by_key(|r| std::cmp::Reverse(r.since_secs));
    finished.sort_by_key(|r| r.since_secs);
    if let Some(n) = limit {
        in_progress.truncate(n);
        waiting.truncate(n);
        finished.truncate(n);
    }
    in_progress
        .into_iter()
        .chain(waiting)
        .chain(finished)
        .collect()
}

/// LSC-2's "forced scrub" for a non-loopback caller (design §6's posture
/// note — this route is `auth_bearer`, NOT loopback-only, unlike
/// `presence`/`live` above; this is the redaction floor that makes that
/// safe): `cwd` collapses to its basename (a full path discloses
/// filesystem structure — exactly why `presence` is loopback-only HARD
/// today) and `last_line` is re-capped server-side even though the hook
/// already caps it client-side (defense in depth against a future or
/// misbehaving adapter). On loopback, both ride in full — see the caller.
fn redact_for_non_loopback(row: &mut LiveStatusRow) {
    if let Some(cwd) = row.cwd.take() {
        let base = std::path::Path::new(&cwd)
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or(cwd);
        row.cwd = Some(base);
    }
    if let Some(ll) = row.last_line.take() {
        row.last_line = Some(kb_core::sessions::truncate_chars_ellipsis(
            &ll,
            kb_core::sessions::OUTCOME_WIRE_MAX_CHARS,
        ));
    }
}

/// `GET /api/sessions/live-status?state=&harness=&project=&limit=` —
/// LSC-2's merged cockpit read (design §6 "Routes"): every registry entry
/// (`source: "hook"`, `confidence: "observed"`) PLUS a Tier-0 degraded
/// layer built from landed captures that have NO registry entry
/// (`source: "capture"`, `confidence: "presumed"`) — so a freshly-restarted
/// daemon is honest rather than empty (design §6). Merge key is
/// `session_id`; a registry entry always wins (a Tier-0 row for a known
/// `session_id` is dropped, never merged/enriched).
///
/// **`auth_bearer`, NOT loopback-only** — the one deliberate departure from
/// `presence`/`live` above (design §6's posture note): this lane serves
/// METADATA ("session X, harness claude, human has held the ball for 49
/// minutes"), not raw transcript bytes, and kb already serves
/// `last_assistant_text` on ordinary authenticated routes. A non-loopback
/// caller gets the SAME rows through [`redact_for_non_loopback`]'s forced
/// scrub floor (design §6's "token + forced scrub" graduation, taken here
/// for the state lane only) rather than a 403 — `ConnectInfo` is read as an
/// axum extractor (the established house pattern for a route HANDLER, not
/// generic `Request`-taking middleware — see `presence` above; invariant
/// #3's "read from extensions" rule targets the latter).
pub async fn live_status(
    State(state): State<Arc<KbHandles>>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Query(q): Query<LiveStatusQuery>,
) -> Response {
    let loopback = crate::middleware::is_loopback_origin(
        Some(peer.ip()),
        &headers,
        &state.origin.trusted_proxies,
    );
    let now_unix = chrono::Utc::now().timestamp();

    let wanted_states: Vec<LiveState> = q
        .state
        .as_deref()
        .map(parse_live_state_csv)
        .unwrap_or_default();
    let wanted_harness: Vec<String> = match q.harness.as_deref().map(parse_harness_csv) {
        Some(Ok(h)) => h,
        Some(Err(resp)) => return resp,
        None => Vec::new(),
    };

    // Tier 1: every registry row, fresh-derived against now_unix.
    let known_ids = state.live_registry.known_session_ids();
    let mut rows: Vec<LiveStatusRow> = state
        .live_registry
        .snapshot(now_unix)
        .into_iter()
        .map(|(r, live_state, confidence)| LiveStatusRow {
            resume: resume_command(&r.harness, &r.session_id),
            session_id: r.session_id,
            harness: r.harness,
            holder: r.holder,
            state: live_state,
            source: StateSource::Hook,
            confidence,
            since_unix: r.last_activity_unix,
            since_secs: (now_unix - r.last_activity_unix).max(0),
            project: derive_project(r.cwd.as_deref()),
            cwd: r.cwd,
            model: r.model,
            title: r.title,
            last_line: r.last_line,
            kb: None,
            artifact_id: None,
            blocked: r.blocked,
            detail_reason: r.detail_reason,
        })
        .collect();

    // Tier 0: recent captures with NO registry entry. Fanned out over
    // state.kbs via buffered_join (invariant #28, never a serial await
    // loop); each per-kb `sessions_list` call is already newest-capture-
    // scoped via `newest_capture_pred` (invariant #11) — no bespoke
    // MAX(started_at) query.
    let known_ids = &known_ids;
    let mut tier0_futs: Vec<super::CorpusFut<'_, Vec<LiveStatusRow>>> = Vec::new();
    for (kb_name, ctx) in state.kbs.iter() {
        tier0_futs.push(Box::pin(async move {
            let list = match ctx
                .storage
                .sessions_list(
                    TIER0_SCAN_LIMIT,
                    None,
                    None,
                    None,
                    None,
                    kb_core::sessions::ProjectFilter::default(),
                    Vec::new(),
                    Vec::new(),
                )
                .await
            {
                Ok(v) => v,
                Err(e) => {
                    tracing::warn!(kb = %kb_name, error = %e, "sessions_list failed in live_status Tier-0 scan");
                    return Vec::new();
                }
            };
            list.into_iter()
                .filter(|row| !known_ids.contains(&row.session_id))
                .filter(|row| {
                    let last = row.started_at.max(row.ended_at);
                    now_unix - last <= kb_core::sessions::ACTIVE_WINDOW_SECS
                })
                .map(|row| {
                    let policy = LivePolicy::default();
                    let last_activity = row.ended_at.max(row.started_at);
                    // A landed capture is a Stop-triggered digest write —
                    // the human just regained control (invariant #11's
                    // multi-capture rule: every Stop fires a capture). This
                    // is the "holder/state derived from the capture's age"
                    // the design calls for (§6): Human, aged by
                    // `derive_state`'s ordinary waiting/cold threshold.
                    let (live_state, confidence) = derive_state(
                        Holder::Human,
                        last_activity,
                        now_unix,
                        StateSource::Capture,
                        &policy,
                    );
                    let last_line = row.last_assistant_text.as_deref().map(|t| {
                        kb_core::sessions::truncate_chars_ellipsis(
                            t,
                            kb_core::sessions::OUTCOME_WIRE_MAX_CHARS,
                        )
                    });
                    LiveStatusRow {
                        resume: resume_command(&row.harness, &row.session_id),
                        session_id: row.session_id,
                        harness: row.harness,
                        holder: Holder::Human,
                        state: live_state,
                        source: StateSource::Capture,
                        confidence,
                        since_unix: last_activity,
                        since_secs: (now_unix - last_activity).max(0),
                        project: row
                            .project_key
                            .clone()
                            .or_else(|| derive_project(row.cwd.as_deref())),
                        cwd: row.cwd,
                        model: row.model,
                        title: row.title,
                        last_line,
                        kb: Some(kb_name.to_string()),
                        artifact_id: Some(row.artifact_id),
                        blocked: false,
                        detail_reason: None,
                    }
                })
                .collect()
        }));
    }
    // PF-R1 — the operator-configurable `[server] fanout_cap` (default 8,
    // byte-identical to the old hardcoded `super::FANOUT_CAP`).
    let tier0: Vec<LiveStatusRow> = super::buffered_join(tier0_futs, state.fanout_cap)
        .await
        .into_iter()
        .flatten()
        .collect();
    rows.extend(tier0);

    if !wanted_states.is_empty() {
        rows.retain(|r| wanted_states.contains(&r.state));
    }
    if !wanted_harness.is_empty() {
        rows.retain(|r| wanted_harness.contains(&r.harness));
    }
    if let Some(p) = q.project.as_deref() {
        rows.retain(|r| {
            r.project
                .as_deref()
                .map(|x| x.eq_ignore_ascii_case(p))
                .unwrap_or(false)
        });
    }

    let mut rows = order_and_cap_live_status_rows(rows, q.limit);
    if !loopback {
        for r in rows.iter_mut() {
            redact_for_non_loopback(r);
        }
    }

    let mut resp = Json(LiveStatusResponse { sessions: rows }).into_response();
    resp.headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    resp
}

#[cfg(test)]
mod lsc2_tests {
    use super::*;

    fn row(state: LiveState, since_secs: i64, session_id: &str) -> LiveStatusRow {
        LiveStatusRow {
            session_id: session_id.to_string(),
            harness: "claude".to_string(),
            holder: match state {
                LiveState::Working | LiveState::Stalled => Holder::Agent,
                LiveState::Waiting | LiveState::Cold => Holder::Human,
                LiveState::Finished | LiveState::PresumedEnded => Holder::Ended,
            },
            state,
            source: StateSource::Hook,
            confidence: kb_core::sessions::live::Confidence::Observed,
            since_unix: 0,
            since_secs,
            project: Some("kb".to_string()),
            cwd: Some("/home/user/project/kb".to_string()),
            model: None,
            title: Some("t".to_string()),
            resume: format!("claude -r {session_id}"),
            last_line: Some("hello world".to_string()),
            kb: None,
            artifact_id: None,
            blocked: false,
            detail_reason: None,
        }
    }

    #[test]
    fn map_event_covers_the_closed_vocabulary_and_rejects_unknowns() {
        assert_eq!(map_event("start"), Some((Holder::Agent, false)));
        assert_eq!(map_event("prompt"), Some((Holder::Agent, false)));
        assert_eq!(map_event("tool"), Some((Holder::Agent, false)));
        assert_eq!(map_event("unblocked"), Some((Holder::Agent, false)));
        assert_eq!(map_event("turn_end"), Some((Holder::Human, false)));
        assert_eq!(map_event("blocked"), Some((Holder::Agent, true)));
        assert_eq!(map_event("end"), Some((Holder::Ended, false)));
        assert_eq!(map_event("bogus"), None);
        assert_eq!(map_event(""), None);
    }

    #[test]
    fn parse_beat_at_accepts_rfc3339_and_bare_unix_seconds() {
        assert_eq!(
            parse_beat_at("2026-08-21T20:41:02Z"),
            Some(
                chrono::DateTime::parse_from_rfc3339("2026-08-21T20:41:02Z")
                    .unwrap()
                    .timestamp()
            )
        );
        assert_eq!(parse_beat_at("1700000000"), Some(1_700_000_000));
        assert_eq!(parse_beat_at("not a timestamp"), None);
        assert_eq!(parse_beat_at(""), None);
    }

    #[test]
    fn derive_project_takes_the_cwd_basename() {
        assert_eq!(
            derive_project(Some("/home/user/project/kb")),
            Some("kb".to_string())
        );
        assert_eq!(
            derive_project(Some("/home/user/project/kb/")),
            Some("kb".to_string())
        );
        assert_eq!(derive_project(None), None);
    }

    #[test]
    fn resume_command_uses_the_verified_claude_form_and_a_placeholder_otherwise() {
        assert_eq!(resume_command("claude", "sid-1"), "claude -r sid-1");
        assert_eq!(resume_command("codex", "sid-1"), "codex resume sid-1");
    }

    #[test]
    fn parse_live_state_csv_drops_unknown_tokens_never_errors() {
        assert_eq!(
            parse_live_state_csv("waiting, cold, bogus"),
            vec![LiveState::Waiting, LiveState::Cold]
        );
        assert!(parse_live_state_csv("").is_empty());
    }

    #[test]
    fn order_and_cap_matches_the_cli_group_lanes_ordering() {
        let rows = vec![
            row(LiveState::Working, 600, "a"),
            row(LiveState::Stalled, 3_000, "b"),
            row(LiveState::Working, 30, "c"),
            row(LiveState::Waiting, 60, "d"),
            row(LiveState::Waiting, 3_000, "e"),
            row(LiveState::Cold, 40_000, "f"),
        ];
        let ordered = order_and_cap_live_status_rows(rows, None);
        let ids: Vec<&str> = ordered.iter().map(|r| r.session_id.as_str()).collect();
        // in_progress most-recently-active-first (c, a, b), then waiting
        // longest-wait-first (e, d), then finished newest-first (f).
        assert_eq!(ids, vec!["c", "a", "b", "e", "d", "f"]);
    }

    #[test]
    fn order_and_cap_limits_each_lane_independently() {
        let rows = vec![
            row(LiveState::Working, 10, "a"),
            row(LiveState::Working, 20, "b"),
            row(LiveState::Waiting, 100, "c"),
        ];
        let ordered = order_and_cap_live_status_rows(rows, Some(1));
        assert_eq!(ordered.len(), 2);
        assert_eq!(ordered[0].session_id, "a");
        assert_eq!(ordered[1].session_id, "c");
    }

    #[test]
    fn redact_for_non_loopback_collapses_cwd_to_basename_and_recaps_last_line() {
        let mut r = row(LiveState::Working, 10, "a");
        r.cwd = Some("/home/user/project/kb".to_string());
        r.last_line = Some("x".repeat(500));
        redact_for_non_loopback(&mut r);
        assert_eq!(r.cwd.as_deref(), Some("kb"));
        assert_eq!(
            r.last_line.as_ref().unwrap().chars().count(),
            kb_core::sessions::OUTCOME_WIRE_MAX_CHARS
        );
    }

    #[test]
    fn redact_for_non_loopback_is_a_noop_on_none_fields() {
        let mut r = row(LiveState::Working, 10, "a");
        r.cwd = None;
        r.last_line = None;
        redact_for_non_loopback(&mut r);
        assert_eq!(r.cwd, None);
        assert_eq!(r.last_line, None);
    }

    #[test]
    fn live_status_row_serializes_with_the_documented_shape() {
        let r = row(LiveState::Waiting, 120, "sid-1");
        let v = serde_json::to_value(&r).unwrap();
        assert_eq!(v["session_id"], serde_json::json!("sid-1"));
        assert_eq!(v["state"], serde_json::json!("waiting"));
        assert_eq!(v["holder"], serde_json::json!("human"));
        assert_eq!(v["source"], serde_json::json!("hook"));
        assert_eq!(v["confidence"], serde_json::json!("observed"));
        assert_eq!(v["resume"], serde_json::json!("claude -r sid-1"));
        // kb/artifact_id are None for a registry-sourced row and must be
        // OMITTED (skip_serializing_if), not emitted as null.
        assert!(v.get("kb").is_none());
        assert!(v.get("artifact_id").is_none());
    }

    #[test]
    fn beat_body_ignores_unknown_top_level_fields() {
        let json = serde_json::json!({
            "v": 1, "session_id": "s1", "harness": "claude", "event": "prompt",
            "at": "2026-08-21T20:41:02Z", "lease_secs": 900,
            "future_field_from_a_newer_hook": {"nested": true},
        });
        let parsed: BeatBody = serde_json::from_value(json).unwrap();
        assert_eq!(parsed.session_id, "s1");
        assert_eq!(parsed.event, "prompt");
    }

    #[test]
    fn beat_body_accepts_the_exact_kb_beat_sh_shape() {
        // Mirrors kb-beat.sh's `jq -n` body construction verbatim (minus
        // dynamic values) — this is the LSC-3 contract this route must
        // match exactly.
        let json = serde_json::json!({
            "v": 1,
            "session_id": "bf283e00-fa36-4a92-899a-980b2ea06d5c",
            "harness": "claude",
            "event": "turn_end",
            "at": "2026-08-21T20:41:02Z",
            "host": "devbox",
            "pid": 1820040,
            "cwd": "/home/user/project/kb",
            "model": "claude-fable-5",
            "lease_secs": 900,
            "last_line": "ping",
        });
        let parsed: BeatBody = serde_json::from_value(json).unwrap();
        assert_eq!(parsed.harness, "claude");
        assert_eq!(parsed.event, "turn_end");
        assert_eq!(parsed.last_line.as_deref(), Some("ping"));
        assert!(parsed.detail.is_none());
        assert!(parsed.title.is_none());
    }
}

#[cfg(test)]
mod live_follow_tests {
    use super::*;

    // Small smoke coverage lives here; the exhaustive tailer/resolver
    // behavior is pinned in `kb_core::sessions::tail`'s own test module —
    // this just confirms the route-layer glue (wire shape, loopback gate
    // wiring, wants_raw_flag) independently.

    #[test]
    fn wants_raw_flag_accepts_the_documented_grammar_only() {
        assert!(wants_raw_flag(&Some("1".to_string())));
        assert!(wants_raw_flag(&Some("true".to_string())));
        assert!(wants_raw_flag(&Some("on".to_string())));
        assert!(!wants_raw_flag(&Some("0".to_string())));
        assert!(!wants_raw_flag(&Some("false".to_string())));
        assert!(!wants_raw_flag(&None));
    }

    #[test]
    fn presence_response_serializes_with_the_documented_shape() {
        let body = PresenceResponse {
            enabled: true,
            live: vec![PresenceEntry {
                session_id: "sid-1".into(),
                mtime_unix: 100,
                bytes: 42,
                project_slug: "-home-user-project-kb".into(),
            }],
        };
        let v = serde_json::to_value(&body).unwrap();
        assert_eq!(v["enabled"], serde_json::json!(true));
        assert_eq!(v["live"][0]["session_id"], serde_json::json!("sid-1"));
        assert_eq!(
            v["live"][0]["project_slug"],
            serde_json::json!("-home-user-project-kb")
        );
    }

    #[test]
    fn scan_live_presence_finds_only_files_within_the_window() {
        let tmp = tempfile::tempdir().unwrap();
        let proj = tmp.path().join("-proj");
        std::fs::create_dir_all(&proj).unwrap();
        std::fs::write(proj.join("fresh.jsonl"), b"{}\n").unwrap();
        // A stale file, backdated past the window.
        let stale = proj.join("stale.jsonl");
        std::fs::write(&stale, b"{}\n").unwrap();
        let old = std::time::SystemTime::now() - std::time::Duration::from_secs(10_000);
        let f = std::fs::OpenOptions::new()
            .write(true)
            .open(&stale)
            .unwrap();
        let _ = f.set_modified(old);

        let live = scan_live_presence(tmp.path(), 120);
        let sids: Vec<&str> = live.iter().map(|e| e.session_id.as_str()).collect();
        assert!(sids.contains(&"fresh"), "{sids:?}");
        assert!(!sids.contains(&"stale"), "{sids:?}");
    }

    #[test]
    fn build_live_delta_raw_mode_returns_decoded_lines_and_no_events() {
        let tmp = tempfile::tempdir().unwrap();
        let p = tmp.path().join("s.jsonl");
        std::fs::write(&p, b"{\"a\":1}\n{\"a\":2}\n").unwrap();
        let cache = crate::live_tail_cache::LiveTailCache::default();
        let resp = build_live_delta("sid-1", &p, 0, true, 120, None, &cache).unwrap();
        assert!(resp.events.is_none());
        assert_eq!(resp.raw_lines.unwrap().len(), 2);
        assert_eq!(resp.parse_failures, 0);
    }

    #[test]
    fn build_live_delta_interpreted_mode_bootstraps_on_a_cold_start() {
        let tmp = tempfile::tempdir().unwrap();
        let p = tmp.path().join("s.jsonl");
        std::fs::write(&p, b"not-json-at-all\n").unwrap();
        let cache = crate::live_tail_cache::LiveTailCache::default();
        let resp = build_live_delta("sid-1", &p, 0, false, 120, None, &cache).unwrap();
        // A cold bootstrap never replays events (LF-4: "the live view is a
        // tail window, not the full document" — the bootstrap only warms
        // the carry's join state).
        assert!(
            resp.events.as_ref().is_some_and(|v| v.is_empty()),
            "{:?}",
            resp.events
        );
        assert!(
            resp.next_from > 0,
            "bootstrap must advance past the seeded line"
        );
    }

    #[test]
    fn build_live_delta_incremental_hits_the_cache_and_emits_new_turns() {
        let tmp = tempfile::tempdir().unwrap();
        let p = tmp.path().join("s.jsonl");
        std::fs::write(&p, b"").unwrap();
        let cache = crate::live_tail_cache::LiveTailCache::default();

        // Seed a carry at offset 0 directly (as if a prior bootstrap landed
        // there with an empty file).
        cache.put(
            "sid-1",
            crate::live_tail_cache::file_inode(&std::fs::metadata(&p).unwrap()),
            0,
            kb_core::sessions::view::ViewCarry::default(),
        );

        // Append one user + one assistant record so a turn actually closes.
        let line1 = serde_json::json!({
            "type": "user",
            "uuid": "11111111-1111-1111-1111-111111111111",
            "timestamp": "2026-01-01T00:00:00Z",
            "message": {"role": "user", "content": "hello"}
        });
        let line2 = serde_json::json!({
            "type": "assistant",
            "uuid": "22222222-2222-2222-2222-222222222222",
            "timestamp": "2026-01-01T00:00:01Z",
            "message": {"role": "assistant", "content": [{"type": "text", "text": "hi"}]}
        });
        std::fs::write(&p, format!("{}\n{}\n", line1, line2)).unwrap();

        let resp = build_live_delta("sid-1", &p, 0, false, 120, None, &cache).unwrap();
        assert!(resp.next_from > 0);
        // At least the user turn must have closed (assistant may still be
        // "open" pending a later non-continuation record — either way this
        // must not error and must report SOME progress).
        assert!(resp.events.is_some());
    }

    /// QTEST-4 — seed the tail cache at offset N, truncate/rewrite the file
    /// shorter, then `build_live_delta` must surface `truncated_restart` and
    /// reboot the carry to a sane `next_from`.
    #[test]
    fn build_live_delta_truncated_restart_reboots_carry() {
        let tmp = tempfile::tempdir().unwrap();
        let p = tmp.path().join("s.jsonl");
        // Write a multi-line file so N is well above zero.
        let long = (0..20)
            .map(|i| format!(r#"{{"type":"user","uuid":"{i:032x}","message":{{"role":"user","content":"x{i}"}}}}"#))
            .collect::<Vec<_>>()
            .join("\n")
            + "\n";
        std::fs::write(&p, &long).unwrap();
        let size_n = std::fs::metadata(&p).unwrap().len();
        assert!(size_n > 0);

        let cache = crate::live_tail_cache::LiveTailCache::default();
        // Warm the cache at the end of the long file (as if a prior poll
        // advanced `next_from` to size).
        let cold = build_live_delta("sid-trunc", &p, 0, false, 120, None, &cache).unwrap();
        assert!(cold.next_from > 0);
        let from_n = cold.next_from;

        // Truncate + rewrite shorter under the same path (inode typically
        // preserved on Linux open/write truncate).
        std::fs::write(&p, b"{\"type\":\"user\",\"uuid\":\"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa\",\"message\":{\"role\":\"user\",\"content\":\"short\"}}\n").unwrap();
        let new_size = std::fs::metadata(&p).unwrap().len();
        assert!(
            new_size < from_n,
            "rewrite must be shorter than prior offset"
        );

        let resp = build_live_delta("sid-trunc", &p, from_n, false, 120, None, &cache).unwrap();
        assert!(
            resp.truncated_restart,
            "truncate fallthrough must set truncated_restart; got {resp:?}"
        );
        // Carry rebooted: next_from is within the new file, not stuck at from_n.
        assert!(
            resp.next_from <= new_size,
            "next_from={} must be ≤ new size {new_size}",
            resp.next_from
        );
        assert!(
            resp.next_from > 0 || new_size == 0,
            "rebooted carry should advance past empty unless the file is empty"
        );
    }

    /// QTEST-11 — empty live dir → empty presence list.
    #[test]
    fn scan_live_presence_empty_dir_returns_empty() {
        let tmp = tempfile::tempdir().unwrap();
        let live = scan_live_presence(tmp.path(), 120);
        assert!(live.is_empty(), "{live:?}");
    }

    /// QTEST-11 — multiple project slugs each contribute their own entries.
    #[test]
    fn scan_live_presence_aggregates_multiple_project_slugs() {
        let tmp = tempfile::tempdir().unwrap();
        for slug in ["-proj-a", "-proj-b"] {
            let proj = tmp.path().join(slug);
            std::fs::create_dir_all(&proj).unwrap();
            std::fs::write(proj.join("sid.jsonl"), b"{}\n").unwrap();
        }
        let live = scan_live_presence(tmp.path(), 120);
        let slugs: std::collections::BTreeSet<&str> =
            live.iter().map(|e| e.project_slug.as_str()).collect();
        assert_eq!(
            slugs,
            ["-proj-a", "-proj-b"].into_iter().collect(),
            "{live:?}"
        );
    }

    /// QTEST-11 — the global scan cap stops the walk after
    /// [`PRESENCE_SCAN_CAP`] directory entries (jsonl or not).
    #[test]
    fn scan_live_presence_respects_scan_cap() {
        let tmp = tempfile::tempdir().unwrap();
        let proj = tmp.path().join("-cap");
        std::fs::create_dir_all(&proj).unwrap();
        // Create more than the cap of tiny jsonl files — empty content is
        // enough; the cap counts directory entries, not bytes.
        let n = PRESENCE_SCAN_CAP + 64;
        for i in 0..n {
            std::fs::write(proj.join(format!("s{i}.jsonl")), b"{}\n").unwrap();
        }
        let live = scan_live_presence(tmp.path(), 120);
        assert!(
            live.len() <= PRESENCE_SCAN_CAP,
            "cap {PRESENCE_SCAN_CAP} exceeded: got {}",
            live.len()
        );
        // With every file fresh and jsonl, the walk should hit the cap and
        // return exactly that many (non-jsonl skips don't apply here).
        assert_eq!(live.len(), PRESENCE_SCAN_CAP);
    }
}

// --- W2/R11/C-16 — the by-artifact join -------------------------------------

#[cfg_attr(feature = "ts-export", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Serialize)]
pub struct ByArtifactResponse {
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts-export", ts(optional))]
    pub session: Option<SessionOut>,
    /// Whether `artifact_id` IS the session's newest capture (compare against
    /// `sessions_get(session_id)` — the SAME newest-capture subquery every
    /// other sessions read uses, invariant #11). `false` when a strictly
    /// newer capture superseded this artifact; always `false` when `session`
    /// is `None`.
    pub newest: bool,
}

/// `GET /api/sessions/by-artifact/{kb}/{artifact_id}` — the canonical
/// artifact→session join (`SessionsGetByArtifactIds`, #11), replacing the
/// SPA's old filename-regex `SessionSelfLink` sid recovery. Single-kb (no
/// fan-out — `kb` is in the path); 404 when the artifact is not a session
/// capture at all (a cheap, cacheable probe every reader-chrome load can
/// afford to make).
pub async fn by_artifact(
    State(state): State<Arc<KbHandles>>,
    Path((kb, artifact_id)): Path<(String, String)>,
) -> Response {
    let (kb_name, ctx) = match super::resolve_kb(&state, &kb) {
        Ok(v) => v,
        Err(r) => return r,
    };
    let rows = ctx
        .storage
        .sessions_get_by_artifact_ids(vec![artifact_id.clone()])
        .await
        .unwrap_or_else(|e| {
            tracing::warn!(kb = %kb_name, artifact_id = %artifact_id, error = %e, "sessions_get_by_artifact_ids failed");
            Vec::new()
        });
    let Some(row) = rows.into_iter().next() else {
        return error_to_problem_json(&kb_core::Error::NotFound(format!(
            "artifact {artifact_id} is not a session capture"
        )))
        .into_response();
    };
    let session_id = row.session_id.clone();
    let mut session = SessionOut::from_row(kb_name.as_str(), row);
    session.memory_count = count_memories_for_session(&state, &session_id).await;
    let newest = ctx
        .storage
        .sessions_get(session_id)
        .await
        .ok()
        .flatten()
        .map(|newest_row| newest_row.artifact_id == artifact_id)
        .unwrap_or(false);
    Json(ByArtifactResponse {
        session: Some(session),
        newest,
    })
    .into_response()
}

// --- W4/R8/ADD-2 — the grokclaude job join (`by-job`) -----------------------

/// Which side of the Claude Code ↔ grokclaude join this match came from.
/// `Driver` — a Claude Code session whose transcript INVOKED the job
/// (`parse_session_activity::extract_grok_job`'s Bash sniffer, live from W4).
/// `Child` — the grokclaude job's OWN capture recording the same ulid at
/// parse time (the grok-side adapter, W5) — the variant exists now so this
/// wire shape never has to change when W5 lands; nothing populates it yet.
#[cfg_attr(feature = "ts-export", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum JobLinkRole {
    Driver,
    Child,
}

#[cfg_attr(feature = "ts-export", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Serialize)]
pub struct JobLinkMatch {
    pub role: JobLinkRole,
    pub session_id: String,
    pub kb: String,
    pub artifact_id: String,
    pub display_name: String,
    pub started_at: i64,
}

#[cfg_attr(feature = "ts-export", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Serialize)]
pub struct ByJobResponse {
    pub ulid: String,
    pub matches: Vec<JobLinkMatch>,
}

/// `GET /api/sessions/by-job/{ulid}` (memo R8/ADD-2) — the grokclaude job
/// join: every session whose transcript recorded a `session_research` row
/// `kind="grok_job"` `query=<ulid>` (today, exclusively the driver-side
/// sniffer — `kind_research::extract_grok_job`), fanned out per corpus (#28),
/// each hit resolved to its session's own newest capture (#11, matching the
/// `by_job`/`session_research_by_job` storage query's own newest-capture
/// scoping). Oldest-first (the driver session that STARTED the job usually
/// wants to sort first; a future multi-round job may accrue several).
/// Which side of a grokclaude job a matched session sits on. The capture
/// whose harness IS `grok` is the job's own transcript (its `grok_job` row
/// comes from the adapter-meta line, W5); any other harness DROVE the job
/// (its row comes from the W4 Bash sniffer). W4 shipped this hardcoded to
/// `Driver` ("child arrives in W5") and W5 added the rows without updating
/// the classification — fixed at the v0.31 milestone gate.
fn job_link_role(harness: &str) -> JobLinkRole {
    if harness == "grok" {
        JobLinkRole::Child
    } else {
        JobLinkRole::Driver
    }
}

pub async fn by_job(State(state): State<Arc<KbHandles>>, Path(ulid): Path<String>) -> Response {
    let ulid = ulid.trim().to_string();
    if ulid.is_empty() {
        return error_to_problem_json(&kb_core::Error::BadRequest("missing job ulid".into()))
            .into_response();
    }
    let ulid_ref = &ulid;
    let mut futs: Vec<super::CorpusFut<'_, Vec<JobLinkMatch>>> = Vec::new();
    for (kb_name, ctx) in state.kbs.iter() {
        futs.push(Box::pin(async move {
            let rows = ctx
                .storage
                .session_research_by_job(ulid_ref.clone())
                .await
                .unwrap_or_else(|e| {
                    tracing::warn!(kb = %kb_name, ulid = %ulid_ref, error = %e, "session_research_by_job failed");
                    Vec::new()
                });
            // One `sessions_get_many` for the whole row set (PSRV-10) instead
            // of a per-row `sessions_get` — newest-capture already applied
            // inside both the research query and `sessions_get_many` (#11).
            let mut session_ids: Vec<String> =
                rows.iter().map(|r| r.session_id.clone()).collect();
            session_ids.sort();
            session_ids.dedup();
            let sessions = ctx
                .storage
                .sessions_get_many(session_ids)
                .await
                .unwrap_or_else(|e| {
                    tracing::warn!(kb = %kb_name, ulid = %ulid_ref, error = %e, "sessions_get_many failed in by_job");
                    Vec::new()
                });
            let by_sid: std::collections::HashMap<String, kb_core::storage::sqlite::SessionRow> =
                sessions
                    .into_iter()
                    .map(|r| (r.session_id.clone(), r))
                    .collect();
            let mut out = Vec::with_capacity(rows.len());
            for row in rows {
                let Some(session_row) = by_sid.get(&row.session_id).cloned() else {
                    continue;
                };
                let started_at = session_row.started_at;
                let session_id = session_row.session_id.clone();
                let role = job_link_role(&session_row.harness);
                let session = SessionOut::from_row(kb_name.as_str(), session_row);
                out.push(JobLinkMatch {
                    role,
                    session_id,
                    kb: kb_name.as_str().to_string(),
                    artifact_id: session.artifact_id,
                    display_name: session.display_name,
                    started_at,
                });
            }
            out
        }));
    }
    // PF-R1 — the operator-configurable `[server] fanout_cap` (default 8,
    // byte-identical to the old hardcoded `super::FANOUT_CAP`).
    let mut matches: Vec<JobLinkMatch> = super::buffered_join(futs, state.fanout_cap)
        .await
        .into_iter()
        .flatten()
        .collect();
    matches.sort_by_key(|m| m.started_at);
    Json(ByJobResponse { ulid, matches }).into_response()
}

#[cfg_attr(
    feature = "ts-export",
    derive(ts_rs::TS),
    ts(export, rename = "SessionReadingRow")
)]
#[derive(Debug, Serialize)]
pub struct ReadingRow {
    pub kb: String,
    pub artifact_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts-export", ts(optional))]
    pub title: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts-export", ts(optional))]
    pub source_relative: Option<String>,
    /// When the human opened the artifact (unix seconds), within the
    /// session's [started_at, ended_at] window.
    pub opened_at: i64,
    /// Furthest scroll % reached (RP-track), absent when no capture.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts-export", ts(optional))]
    pub read_pct: Option<u8>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts-export", ts(optional))]
    pub last_section: Option<String>,
}

#[cfg_attr(
    feature = "ts-export",
    derive(ts_rs::TS),
    ts(export, rename = "SessionReadingsResponse")
)]
#[derive(Debug, Serialize)]
pub struct ReadingsResponse {
    pub readings: Vec<ReadingRow>,
}

/// `GET /api/sessions/{session_id}/readings` — what the HUMAN read in the SPA
/// during the session's time window, the read-counterpart to `/touches`
/// (which is what the AGENT *referenced* in the transcript — a different
/// actor and a different source). Fans out `history_opens_in_window` across
/// every kb, enriches with title + reading %, dedups `(kb, artifact_id)`
/// keeping the newest, and returns newest-first. NOTE: `ended_at` is the
/// transcript's capture mtime, so the window is "session start → capture",
/// not a precise session end.
pub async fn readings(
    State(state): State<Arc<KbHandles>>,
    Path(session_id): Path<String>,
) -> impl IntoResponse {
    let Some(session) = find_session(&state, &session_id).await else {
        return error_to_problem_json(&kb_core::Error::NotFound(format!("session {session_id}")));
    };
    let (from, to) = (session.started_at, session.ended_at);
    // FF-D — fan out per kb; the (kb, artifact_id) dedup is per-corpus (kb is
    // constant within a corpus), so each future dedups its own opens by
    // artifact_id and the fold concatenates in BTreeMap order before the sort.
    // v0.34 Y1 — UNCHANGED all-users by design (narrates team reading).
    // Opens are not filtered by user; rollup enrichment uses the configured
    // operator as a display default for read% (not per-request Identity).
    let operator = state.operator_user().to_string();
    let mut futs: Vec<super::CorpusFut<'_, Vec<ReadingRow>>> = Vec::new();
    for (kb_name, ctx) in state.kbs.iter() {
        let operator = operator.clone();
        futs.push(Box::pin(async move {
            let opens = match ctx.storage.history_opens_in_window(from, to, 500).await {
                Ok(v) => v,
                Err(e) => {
                    tracing::warn!(kb = %kb_name, error = %e, "history_opens_in_window failed");
                    return Vec::new();
                }
            };
            let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
            let mut rows: Vec<ReadingRow> = Vec::new();
            for row in opens {
                let Some(artifact_id) = row.artifact_id else {
                    continue;
                };
                // Rows are newest-first per kb, so the first occurrence of an
                // artifact_id is the most recent open in the window.
                if !seen.insert(artifact_id.clone()) {
                    continue;
                }
                let (title, source_relative) =
                    match ctx.storage.get_by_id(artifact_id.clone()).await {
                        Ok(Some(d)) => (
                            Some(d.title),
                            Some(kb_core::paths::doc_rel_path(&d.path, &ctx.source_path)),
                        ),
                        _ => (None, None),
                    };
                let (read_pct, last_section) = if ctx.reading_progress {
                    match ctx
                        .storage
                        .reading_latest_for_artifact(artifact_id.clone(), operator.clone())
                        .await
                    {
                        Ok(Some((pct, last, _))) => (Some(pct), last),
                        _ => (None, None),
                    }
                } else {
                    (None, None)
                };
                rows.push(ReadingRow {
                    kb: kb_name.as_str().to_string(),
                    artifact_id,
                    title,
                    source_relative,
                    opened_at: row.started_at_unix,
                    read_pct,
                    last_section,
                });
            }
            rows
        }));
    }
    // PF-R1 — the operator-configurable `[server] fanout_cap` (default 8,
    // byte-identical to the old hardcoded `super::FANOUT_CAP`).
    let mut rows: Vec<ReadingRow> = super::buffered_join(futs, state.fanout_cap)
        .await
        .into_iter()
        .flatten()
        .collect();
    rows.sort_by_key(|b| std::cmp::Reverse(b.opened_at));
    Json(ReadingsResponse { readings: rows }).into_response()
}

// --- helpers ---------------------------------------------------------------

/// Sum the `count_docs_with_kb_session` filter scan across every kb.
/// Returns 0 when every kb errored — the SPA renders that as "no
/// memories yet" rather than surfacing a per-kb failure on the
/// session row.
async fn count_memories_for_session(state: &Arc<KbHandles>, session_id: &str) -> u64 {
    let mut futs: Vec<super::CorpusFut<'_, u64>> = Vec::new();
    for (_, ctx) in state.kbs.iter() {
        futs.push(Box::pin(async move {
            match ctx
                .storage
                .count_docs_with_kb_session(session_id.to_string())
                .await
            {
                Ok(n) => n,
                Err(e) => {
                    tracing::warn!(
                        session_id = %session_id,
                        error = %e,
                        "count_docs_with_kb_session failed",
                    );
                    0
                }
            }
        }));
    }
    // PF-R1 — the operator-configurable `[server] fanout_cap` (default 8,
    // byte-identical to the old hardcoded `super::FANOUT_CAP`).
    super::buffered_join(futs, state.fanout_cap)
        .await
        .into_iter()
        .sum()
}

/// R7 — the query text for `recollect --similar-to`: the source session's
/// indexed digest excerpt (`DocSummary.summary`, populated by R1 with
/// `session_digest_excerpt`). Resolved from the session's owning corpus
/// (`sessions_get` → `artifact_id` → `get_by_id(...).summary`), so it's the
/// exact representation the index matches against — no transcript re-read
/// (#27), deterministic + LLM-free (#10). `None` when no corpus holds the
/// session or its row carries no digest summary.
async fn resolve_session_digest_query(state: &Arc<KbHandles>, session_id: &str) -> Option<String> {
    let mut futs: Vec<super::CorpusFut<'_, Option<String>>> = Vec::new();
    for (kb_name, ctx) in state.kbs.iter() {
        futs.push(Box::pin(async move {
            let row = match ctx.storage.sessions_get(session_id.to_string()).await {
                Ok(Some(r)) => r,
                Ok(None) => return None,
                Err(e) => {
                    tracing::warn!(kb = %kb_name, session_id, error = %e, "sessions_get failed");
                    return None;
                }
            };
            match ctx.storage.get_by_id(row.artifact_id.clone()).await {
                Ok(Some(doc)) => doc.summary,
                _ => None,
            }
        }));
    }
    // PF-R1 — the operator-configurable `[server] fanout_cap` (default 8,
    // byte-identical to the old hardcoded `super::FANOUT_CAP`).
    super::buffered_join(futs, state.fanout_cap)
        .await
        .into_iter()
        .flatten()
        .next()
}

/// Look up the enrichment row for a session id, scanning every kb.
/// Returns the first match (by BTreeMap order, then by sqlite's
/// `sessions_get` tiebreak). Skips kbs that don't carry the row;
/// returns `None` only when every kb has been checked and none
/// matched.
/// Locate a session by id across all corpora WITHOUT the memory-count
/// fan-out. The reverse endpoint (`artifact_sessions`) calls this once per
/// touching session, so paying the second count fan-out per session (whose
/// result it then discards) would be pure waste.
async fn lookup_session(state: &Arc<KbHandles>, session_id: &str) -> Option<SessionOut> {
    // FF-D — fan out the lookup; take the first match in BTreeMap order (same
    // as the serial scan). `title` is the persisted V0017 column now (no lance
    // read); the non-owning corpora return None early.
    let mut futs: Vec<super::CorpusFut<'_, Option<SessionOut>>> = Vec::new();
    for (kb_name, ctx) in state.kbs.iter() {
        futs.push(Box::pin(async move {
            let row = match ctx.storage.sessions_get(session_id.to_string()).await {
                Ok(Some(r)) => r,
                // Either no row in this kb (Ok(None)) or a transient
                // lookup error — skip this corpus either way.
                Ok(None) => return None,
                Err(e) => {
                    tracing::warn!(
                        kb = %kb_name,
                        session_id = %session_id,
                        error = %e,
                        "sessions_get failed",
                    );
                    return None;
                }
            };
            Some(SessionOut::from_row(kb_name.as_str(), row))
        }));
    }
    // PF-R1 — the operator-configurable `[server] fanout_cap` (default 8,
    // byte-identical to the old hardcoded `super::FANOUT_CAP`).
    super::buffered_join(futs, state.fanout_cap)
        .await
        .into_iter()
        .flatten()
        .next()
}

/// Locate a session AND fill its `memory_count` (the cross-kb count fan-out).
/// Used by the single-session detail routes where the count is surfaced.
async fn find_session(state: &Arc<KbHandles>, session_id: &str) -> Option<SessionOut> {
    let mut found = lookup_session(state, session_id).await;
    if let Some(so) = found.as_mut() {
        so.memory_count = count_memories_for_session(state, &so.session_id).await;
    }
    found
}

#[cfg(test)]
mod replay_tests {
    use super::*;
    use kb_core::sessions::replay::{ReplayBeat, ReplayKind};

    fn beat(seq: usize, kind: ReplayKind, path: Option<&str>) -> ReplayBeat {
        ReplayBeat {
            seq,
            ts_unix: 1_000 + seq as i64,
            delta_secs: if seq == 0 { 0 } else { 1 },
            kind,
            detail: format!("d{seq}"),
            path: path.map(|p| p.to_string()),
            line_range: None,
            snippet: None,
            count: 1,
            turn: None,
        }
    }

    fn out(seq: usize, artifact: Option<&str>) -> ReplayBeatOut {
        ReplayBeatOut {
            beat: beat(seq, ReplayKind::Read, Some("/p/a.html")),
            kb: artifact.map(|_| "docs".to_string()),
            artifact_id: artifact.map(|a| a.to_string()),
            source_relative: artifact.map(|_| "a.html".to_string()),
            heading_slug: None,
        }
    }

    #[test]
    fn no_filters_returns_everything() {
        let beats = vec![out(0, Some("aaa")), out(1, None), out(2, Some("bbb"))];
        let (w, matched, window) = filter_replay_beats(&beats, None, 0, usize::MAX);
        assert_eq!(w.len(), 3);
        assert_eq!(matched, 3);
        assert_eq!(window.from_seq, 0);
        assert_eq!(window.returned, 3);
        assert_eq!(window.total, 3);
    }

    #[test]
    fn artifact_filter_keeps_only_that_artifact() {
        let beats = vec![
            out(0, Some("aaa")),
            out(1, None),
            out(2, Some("bbb")),
            out(3, Some("aaa")),
        ];
        let (w, matched, _window) = filter_replay_beats(&beats, Some("aaa"), 0, usize::MAX);
        assert_eq!(matched, 2);
        assert_eq!(
            w.iter().map(|b| b.beat.seq).collect::<Vec<_>>(),
            vec![0, 3],
            "transcript order is preserved through the filter"
        );
    }

    /// An unresolved beat (out-of-corpus path) is never matched by
    /// `?artifact=`, but it is also never dropped from an unfiltered timeline.
    #[test]
    fn unresolved_beats_survive_but_never_match_an_artifact_filter() {
        let beats = vec![out(0, None), out(1, Some("aaa"))];
        let (all, _, _) = filter_replay_beats(&beats, None, 0, usize::MAX);
        assert_eq!(all.len(), 2);
        let (w, matched, _window) = filter_replay_beats(&beats, Some("aaa"), 0, usize::MAX);
        assert_eq!(matched, 1);
        assert_eq!(w[0].beat.seq, 1);
    }

    #[test]
    fn limit_windows_after_the_artifact_filter_and_matched_stays_honest() {
        let beats = vec![
            out(0, Some("aaa")),
            out(1, Some("bbb")),
            out(2, Some("aaa")),
        ];
        let (w, matched, window) = filter_replay_beats(&beats, Some("aaa"), 0, 1);
        assert_eq!(w.len(), 1);
        assert_eq!(w[0].beat.seq, 0);
        assert_eq!(
            matched, 2,
            "matched counts pre-limit, so a window is visible as a window"
        );
        assert_eq!(window.returned, 1);
        assert_eq!(window.total, 2);
    }

    #[test]
    fn limit_zero_returns_no_beats_but_still_reports_matched() {
        let beats = vec![out(0, Some("aaa"))];
        let (w, matched, window) = filter_replay_beats(&beats, None, 0, 0);
        assert!(w.is_empty());
        assert_eq!(matched, 1);
        assert_eq!(window.returned, 0);
    }

    /// R7/S6 — `from_seq` slices AFTER the artifact filter but BEFORE limit:
    /// the serve-window offset into the matched set.
    #[test]
    fn from_seq_offsets_into_the_matched_set() {
        let beats = vec![
            out(0, Some("aaa")),
            out(1, Some("aaa")),
            out(2, Some("aaa")),
            out(3, Some("aaa")),
        ];
        let (w, matched, window) = filter_replay_beats(&beats, Some("aaa"), 2, usize::MAX);
        assert_eq!(w.iter().map(|b| b.beat.seq).collect::<Vec<_>>(), vec![2, 3]);
        assert_eq!(matched, 4);
        assert_eq!(window.from_seq, 2);
        assert_eq!(window.returned, 2);
        assert_eq!(window.total, 4);
    }

    /// The cache-key discriminator must be stable AND distinct per posture —
    /// invariant #4 leans on it (see `crate::replay_cache`).
    #[test]
    fn scrub_tag_is_stable_and_posture_distinct() {
        use kb_core::session_scrub::ScrubOptions;
        let none = ScrubOptions::default();
        let secrets = ScrubOptions::secrets_only();
        let all = ScrubOptions {
            secrets: true,
            paths: true,
            entropy: true,
        };
        assert_eq!(scrub_tag(&none), "");
        assert_eq!(scrub_tag(&secrets), "s");
        assert_eq!(scrub_tag(&all), "spe");
        assert_ne!(scrub_tag(&none), scrub_tag(&secrets));
        assert_eq!(
            scrub_tag(&secrets),
            scrub_tag(&ScrubOptions::secrets_only())
        );
    }

    #[test]
    fn job_link_role_classifies_by_harness() {
        // The grok-harness capture IS the job (child); everything else —
        // claude, codex, opencode, or any future harness — drove it.
        assert_eq!(job_link_role("grok"), JobLinkRole::Child);
        assert_eq!(job_link_role("claude"), JobLinkRole::Driver);
        assert_eq!(job_link_role("codex"), JobLinkRole::Driver);
        assert_eq!(job_link_role("opencode"), JobLinkRole::Driver);
    }
}
