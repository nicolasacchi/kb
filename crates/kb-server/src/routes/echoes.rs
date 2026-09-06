//! `GET /api/kb/{kb}/echoes` — on-this-day: a deterministic, pull-only,
//! date-join surface across three existing signals (created / read /
//! worked-on), rendered as a calm SECOND strip beside the resurface queue.
//!
//! A separate endpoint on purpose (not a `kind` on `ResurfaceItem`): the
//! resurface scorer's `comment_term + read_term == score` explain contract is
//! golden-pinned (`kb_core::resurface`) and the CLI/SPA both hardcode that
//! two-term arithmetic, so an echo card — scored by date proximity, not the
//! comment/read formula — must not ride through it. This route clones the
//! resurface skeleton instead: `resolve_kb` → a handful of read-only storage
//! calls → pure, injected-clock ranking/building → `Json`. No queue rows, no
//! dismiss state, no counters, no writes, no SSE, no storage-generation
//! bumps — a VIEW over `docs`/`history`/`sessions`, exactly like resurface.
//!
//! "Today" is the UTC calendar date (`chrono::Utc::now().date_naive()`) —
//! this codebase has no per-user timezone concept anywhere (one-daemon,
//! one-operator; see root CLAUDE.md non-goals), so UTC is the only
//! deterministic, reproducible-across-machines choice, matching every other
//! unix timestamp in kb.
//!
//! Anniversaries considered: 1/2/3 years back plus a 6-month half-anniversary,
//! all "same day-of-month" arithmetic via `chrono::NaiveDate::from_ymd_opt`,
//! which returns `None` (skip, never clamp) when the target day doesn't exist
//! — the one whole-year case is 29 Feb landing in a non-leap target year; the
//! half-year case is any day-of-month that doesn't exist 6 months back (e.g.
//! 31 Aug → "31 Feb"). Both policies are unit-pinned below.
//!
//! Field grammar: ONE distance field, `months_ago` (6/12/24/36), rather than
//! a `years_ago`-with-a-half-year-exception — simpler for both the sort key
//! and the SPA chip.

use crate::middleware::error_to_problem_json;
use crate::state::KbHandles;
use axum::{
    body::Body,
    extract::{Path, Query, State},
    http::Response,
    response::IntoResponse,
    Json,
};
use chrono::{Datelike, NaiveDate};
use kb_core::storage::sqlite::{HistoryRow, SessionRow};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::Arc;

/// Default page size. The SPA strip renders only the top 2. Hard cap keeps
/// an oversized `?limit=` bounded.
const DEFAULT_LIMIT: u32 = 4;
const MAX_LIMIT: u32 = 12;
/// Per-anniversary-day cap on the `history_opens_in_window` read — a day
/// bucket is a single calendar day, so this is generous headroom, not a
/// meaningful truncation in practice (cf. the 500-row fan-out cap
/// `routes/sessions.rs` uses for a much wider window).
const HISTORY_WINDOW_CAP: u32 = 200;

#[cfg_attr(feature = "ts-export", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Serialize, Clone, Copy, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum EchoKind {
    Created,
    Worked,
    Read,
}

#[cfg_attr(feature = "ts-export", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Serialize)]
pub struct EchoOut {
    pub kind: EchoKind,
    /// Distance from "today": 6, 12, 24, or 36 (the half-year and the three
    /// whole-year anniversaries).
    pub months_ago: u8,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts-export", ts(optional))]
    pub artifact_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts-export", ts(optional))]
    pub source_relative: Option<String>,
    pub title: String,
    /// Server-built, deterministic sentence — e.g. "read 3 times this day
    /// last year" or "kb · 5 files edited · 6 months ago".
    pub detail: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts-export", ts(optional))]
    pub session_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts-export", ts(optional))]
    pub started_at: Option<i64>,
}

/// CT-E6 — a memory's status as of "today", surfaced honestly rather than
/// silently dropped the way `memory::rerank` drops a superseded/forgotten
/// hit from recall. This lane is reflection, not ranking.
#[cfg_attr(feature = "ts-export", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Serialize, Clone, Copy, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum BeliefStatus {
    Active,
    Superseded,
    Forgotten,
}

/// CT-E6 — one belief-lane row: a memory whose CREATED date lands on
/// today's anniversary.
#[cfg_attr(feature = "ts-export", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Serialize)]
pub struct BeliefOut {
    pub id: String,
    pub kb: String,
    pub title: String,
    /// `created_unix`, matching `EchoKind::Created`'s own date-join field
    /// (never `mtime_unix` — same honesty rule as `created_echoes`).
    pub created: i64,
    /// Same distance grammar as `EchoOut::months_ago` (6/12/24/36).
    pub months_ago: u8,
    pub status: BeliefStatus,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts-export", ts(optional))]
    pub superseded_by: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts-export", ts(optional))]
    pub superseded_at: Option<i64>,
}

#[cfg_attr(feature = "ts-export", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Serialize)]
pub struct EchoesResponse {
    pub items: Vec<EchoOut>,
    /// The UTC calendar date every anniversary was computed against
    /// (`YYYY-MM-DD`) — lets a long-open tab detect it has crossed a day
    /// boundary, mirroring resurface's `now_unix`.
    pub today: String,
    /// CT-E6 — additive beliefs lane (memory-category docs on an
    /// anniversary day). Empty on a kb with no memory-category docs at
    /// all, exactly like `items` on a kb with no anniversary hits.
    pub beliefs: Vec<BeliefOut>,
    /// CT-E6 / MI-W2.4c — EPOCH HONESTY caveat, present only when the
    /// anniversary window this response queried reaches back before this
    /// daemon's tombstone era. Same wording pattern `kb memory log` prints
    /// (`routes::memory::tombstone_era` / `commands::memory::render_lineage`).
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts-export", ts(optional))]
    pub beliefs_tombstone_caveat: Option<String>,
}

#[derive(Debug, Deserialize, Default)]
pub struct EchoesParams {
    pub limit: Option<u32>,
}

pub async fn list(
    State(state): State<Arc<KbHandles>>,
    Path(kb): Path<String>,
    Query(params): Query<EchoesParams>,
) -> Response<Body> {
    let (kb_name, ctx) = match crate::routes::resolve_kb(&state, &kb) {
        Ok(v) => v,
        Err(resp) => return resp,
    };
    let limit = resolve_limit(params.limit);
    let today = chrono::Utc::now().date_naive();
    let buckets = anniversary_buckets(today);

    // Doc metadata in ONE scan (the facets/resurface pattern — list_docs
    // always projects `created_unix`, so no per-item round-trip). Memory /
    // session-transcript docs are excluded from `docs_meta` (the
    // created/read lanes), matching resurface: they have their own
    // attention economies (recall / the sessions worklog). The SAME scan
    // feeds the CT-E6 beliefs lane below (`docs` stays alive; no second
    // storage round-trip, no fan-out — see that section's doc comment).
    let docs = match ctx.storage.list_docs(u32::MAX).await {
        Ok(r) => r,
        Err(e) => return error_to_problem_json(&e),
    };
    let mut docs_meta: HashMap<String, DocMeta> = HashMap::new();
    for doc in &docs {
        if doc
            .kb_category
            .as_deref()
            .is_some_and(|c| c.starts_with("memory"))
        {
            continue;
        }
        let rel = kb_core::paths::doc_rel_path(&doc.path, &ctx.source_path);
        docs_meta.insert(
            doc.id.clone(),
            DocMeta {
                title: doc.title.clone(),
                source_relative: rel,
                created_unix: doc.created_unix,
            },
        );
    }

    let mut items = created_echoes(&buckets, &docs_meta);

    // One `history_opens_in_window` round-trip per anniversary day — small,
    // indexed range scans (idx_history_started), never a full-table scan.
    for bucket in &buckets {
        let (day_start, day_end) = day_bounds_unix(bucket.date);
        let rows = match ctx
            .storage
            .history_opens_in_window(day_start, day_end - 1, HISTORY_WINDOW_CAP)
            .await
        {
            Ok(r) => r,
            Err(e) => return error_to_problem_json(&e),
        };
        let counts = aggregate_opens(&rows);
        items.extend(read_echoes_for_bucket(bucket, &counts, &docs_meta));
    }

    // Sessions — one full-table scan (mirrors `list_docs(u32::MAX)` above;
    // `sessions_list` already collapses multi-capture in SQL, #11, but
    // `collapse_newest_capture` below re-applies it defensively/purely so
    // the policy is unit-testable without a DB).
    let sessions = match ctx
        .storage
        .sessions_list(
            u32::MAX,
            None,
            None,
            None,
            None,
            Default::default(),
            Vec::new(),
            Vec::new(),
        )
        .await
    {
        Ok(r) => r,
        Err(e) => return error_to_problem_json(&e),
    };
    items.extend(worked_echoes(&buckets, sessions));

    // CT-E6 — the beliefs lane. `echoes` is per-kb only (no `scope=all` —
    // see the module doc comment), so this stays per-kb too: no invariant
    // #28 fan-out to add, no new storage round-trip either (reuses `docs`
    // above). "Memory-scoped" has no first-class flag in `KbConfig` to
    // check, so this infers it the same way the `docs_meta` loop above
    // does and `lib::backfill_memory_links_seed`: a kb with at least one
    // `"memory-*"`-except-`memory-session` doc IS memory-scoped for this
    // lane's purposes; a kb with none gets an empty lane and no caveat.
    let has_memory_docs = docs
        .iter()
        .any(|d| is_memory_category(d.kb_category.as_deref()));
    let mut beliefs = belief_echoes(&buckets, kb_name.as_str(), &docs);
    beliefs.sort_by(|a, b| {
        a.months_ago
            .cmp(&b.months_ago)
            .then_with(|| a.id.cmp(&b.id))
    });
    let beliefs_tombstone_caveat =
        tombstone_caveat_if_needed(&buckets, has_memory_docs, state.tombstone_era_started_unix);

    let items = finalize(items, limit);
    Json(EchoesResponse {
        items,
        today: today.format("%Y-%m-%d").to_string(),
        beliefs,
        beliefs_tombstone_caveat,
    })
    .into_response()
}

fn resolve_limit(requested: Option<u32>) -> usize {
    requested.unwrap_or(DEFAULT_LIMIT).clamp(1, MAX_LIMIT) as usize
}

struct DocMeta {
    title: String,
    source_relative: String,
    created_unix: Option<i64>,
}

/// One target anniversary: a calendar date plus its distance in months.
#[derive(Debug, Clone, Copy)]
struct DayBucket {
    date: NaiveDate,
    months_ago: u8,
}

/// `[day_start, day_end)` unix bounds of a UTC calendar day.
fn day_bounds_unix(date: NaiveDate) -> (i64, i64) {
    let start = date
        .and_hms_opt(0, 0, 0)
        .expect("midnight is always a valid time")
        .and_utc()
        .timestamp();
    (start, start + 86_400)
}

fn date_of(unix: i64) -> Option<NaiveDate> {
    chrono::DateTime::from_timestamp(unix, 0).map(|dt| dt.date_naive())
}

/// Same day-of-month `months_back` months before `base`, or `None` when that
/// day doesn't exist in the target month (no clamping — `from_ymd_opt`'s own
/// validity gate IS the "trivially clean" test).
fn shift_months(base: NaiveDate, months_back: i64) -> Option<NaiveDate> {
    let total = i64::from(base.year()) * 12 + i64::from(base.month()) - 1 - months_back;
    let y = total.div_euclid(12) as i32;
    let m = (total.rem_euclid(12) + 1) as u32;
    NaiveDate::from_ymd_opt(y, m, base.day())
}

/// Build the anniversary set for `today`: a 6-month half-anniversary (only
/// when trivially clean, no clamping) plus whole-year 1/2/3 anniversaries
/// (skipped, never drifted, when `today` is 29 Feb and the target year isn't
/// leap).
fn anniversary_buckets(today: NaiveDate) -> Vec<DayBucket> {
    let mut buckets = Vec::new();
    if let Some(d) = shift_months(today, 6) {
        buckets.push(DayBucket {
            date: d,
            months_ago: 6,
        });
    }
    for years in 1..=3u8 {
        if let Some(d) =
            NaiveDate::from_ymd_opt(today.year() - i32::from(years), today.month(), today.day())
        {
            buckets.push(DayBucket {
                date: d,
                months_ago: years * 12,
            });
        }
    }
    buckets
}

fn age_phrase(months_ago: u8) -> String {
    match months_ago {
        6 => "6 months ago".to_string(),
        12 => "last year".to_string(),
        n if n % 12 == 0 => format!("{} years ago", n / 12),
        n => format!("{n} months ago"),
    }
}

fn times_phrase(count: u32) -> String {
    if count == 1 {
        "once".to_string()
    } else {
        format!("{count} times")
    }
}

/// Basename of a working-directory path — a local twin of
/// `routes::sessions::folder_basename` (private to that module); duplicated
/// rather than exposed cross-module, matching the small-pure-helper locality
/// this route family already uses (e.g. resurface's own `fmt_age`).
fn cwd_basename(cwd: &str) -> String {
    cwd.trim_end_matches('/')
        .rsplit('/')
        .next()
        .unwrap_or(cwd)
        .to_string()
}

/// The display-name ladder (title → first_user_prompt → short id) — a local
/// twin of `routes::sessions::display_name_of` (private to that module).
fn session_title(row: &SessionRow) -> String {
    row.title
        .as_deref()
        .filter(|t| !t.trim().is_empty())
        .or_else(|| {
            row.first_user_prompt
                .as_deref()
                .filter(|p| !p.trim().is_empty())
        })
        .map(str::to_string)
        .unwrap_or_else(|| {
            let short: String = row.session_id.chars().take(8).collect();
            format!("session {short}")
        })
}

/// #11 — collapse multi-capture sessions to the NEWEST capture per
/// `session_id` (`started_at` desc, `artifact_id` asc tiebreak — the same
/// rule `sessions_list`'s own SQL enforces). Defensive/pure here so the
/// day-bucket join has one deterministic input per session and the policy is
/// unit-testable without a DB.
fn collapse_newest_capture(rows: Vec<SessionRow>) -> Vec<SessionRow> {
    let mut newest: HashMap<String, SessionRow> = HashMap::new();
    for row in rows {
        newest
            .entry(row.session_id.clone())
            .and_modify(|existing| {
                if is_newer_capture(&row, existing) {
                    *existing = row.clone();
                }
            })
            .or_insert(row);
    }
    newest.into_values().collect()
}

fn is_newer_capture(a: &SessionRow, b: &SessionRow) -> bool {
    a.started_at > b.started_at || (a.started_at == b.started_at && a.artifact_id < b.artifact_id)
}

/// Signal 1 — docs whose `created_unix` (never `mtime_unix` — honesty: a doc
/// with no captured btime is skipped entirely, not faked) falls on an
/// anniversary day.
fn created_echoes(buckets: &[DayBucket], docs_meta: &HashMap<String, DocMeta>) -> Vec<EchoOut> {
    let target: HashMap<NaiveDate, u8> = buckets.iter().map(|b| (b.date, b.months_ago)).collect();
    docs_meta
        .iter()
        .filter_map(|(id, meta)| {
            let created = meta.created_unix?;
            let date = date_of(created)?;
            let months_ago = *target.get(&date)?;
            Some(EchoOut {
                kind: EchoKind::Created,
                months_ago,
                artifact_id: Some(id.clone()),
                source_relative: Some(meta.source_relative.clone()),
                title: meta.title.clone(),
                detail: format!("created this day {}", age_phrase(months_ago)),
                session_id: None,
                started_at: None,
            })
        })
        .collect()
}

/// Signal 2 — per-artifact open-visit counts within one anniversary day's
/// window (rows already filtered to `kind='open'` by
/// `history_opens_in_window`).
fn aggregate_opens(rows: &[HistoryRow]) -> HashMap<String, u32> {
    let mut out: HashMap<String, u32> = HashMap::new();
    for r in rows {
        if let Some(id) = &r.artifact_id {
            *out.entry(id.clone()).or_insert(0) += 1;
        }
    }
    out
}

fn read_echoes_for_bucket(
    bucket: &DayBucket,
    counts: &HashMap<String, u32>,
    docs_meta: &HashMap<String, DocMeta>,
) -> Vec<EchoOut> {
    counts
        .iter()
        .filter_map(|(id, count)| {
            let meta = docs_meta.get(id)?;
            Some(EchoOut {
                kind: EchoKind::Read,
                months_ago: bucket.months_ago,
                artifact_id: Some(id.clone()),
                source_relative: Some(meta.source_relative.clone()),
                title: meta.title.clone(),
                detail: format!(
                    "read {} this day {}",
                    times_phrase(*count),
                    age_phrase(bucket.months_ago)
                ),
                session_id: None,
                started_at: None,
            })
        })
        .collect()
}

/// Signal 3 — sessions whose `started_at` falls on an anniversary day.
fn worked_echoes(buckets: &[DayBucket], sessions: Vec<SessionRow>) -> Vec<EchoOut> {
    let target: HashMap<NaiveDate, u8> = buckets.iter().map(|b| (b.date, b.months_ago)).collect();
    collapse_newest_capture(sessions)
        .into_iter()
        .filter_map(|row| {
            let date = date_of(row.started_at)?;
            let months_ago = *target.get(&date)?;
            let title = session_title(&row);
            let cwd = row
                .cwd
                .as_deref()
                .map(cwd_basename)
                .unwrap_or_else(|| "session".to_string());
            let plural = if row.files_edited_count == 1 { "" } else { "s" };
            Some(EchoOut {
                kind: EchoKind::Worked,
                months_ago,
                artifact_id: None,
                source_relative: None,
                title,
                detail: format!(
                    "{cwd} · {} file{plural} edited · {}",
                    row.files_edited_count,
                    age_phrase(months_ago)
                ),
                session_id: Some(row.session_id.clone()),
                started_at: Some(row.started_at),
            })
        })
        .collect()
}

/// CT-E6 — the same `"memory-*"`-except-`memory-session` gate
/// `lib::backfill_memory_links_seed`'s `is_memory` check uses: a memory
/// need not carry this prefix to participate in recall
/// (`memory::is_recallable_memory_category` is the broader predicate for
/// that), but the beliefs lane is explicitly scoped to the narrower,
/// unambiguous "this IS a memory artifact" set per the CT-E6 brief.
fn is_memory_category(category: Option<&str>) -> bool {
    category.is_some_and(|c| {
        c.starts_with("memory-") && c != kb_core::sessions::MEMORY_SESSION_CATEGORY
    })
}

/// Reverse `kb_supersedes` pointer, keyed by the SUPERSEDED (target) id —
/// built from the SAME full-kb `list_docs` scan already in hand, unlike
/// the on-demand `find_superseded_by` the lineage route calls per hop.
/// Ties (more than one doc claiming to supersede the same predecessor —
/// nothing at write time prevents that) resolve to the same deterministic
/// winner `Storage::find_superseded_by` sorts to: `created_unix`
/// ascending (`None` first), then `id` ascending.
fn superseded_by_map(
    docs: &[kb_core::storage::lance::DocSummary],
) -> HashMap<&str, &kb_core::storage::lance::DocSummary> {
    let mut map: HashMap<&str, &kb_core::storage::lance::DocSummary> = HashMap::new();
    for d in docs {
        let Some(target) = d.kb_supersedes.as_deref() else {
            continue;
        };
        let is_earlier = |a: &kb_core::storage::lance::DocSummary| {
            (a.created_unix, a.id.as_str()) < (d.created_unix, d.id.as_str())
        };
        match map.get(target) {
            Some(existing) if is_earlier(existing) => {}
            _ => {
                map.insert(target, d);
            }
        }
    }
    map
}

/// Signal 4 (CT-E6) — the beliefs lane: memory-category docs whose
/// CREATED date lands on an anniversary day. Surfaced-never-scored: every
/// match is returned regardless of salience/decay/pin state — this is
/// reflection, not recall, so nothing here is dropped the way
/// `memory::rerank` drops a low-salience, superseded, or forgotten
/// memory. Status is derived, never stored: `forgotten` from `kb_status`
/// (MI-W2.3's soft-forget tombstone) takes priority over `superseded`
/// (a forgotten memory that also has a successor is still shown as
/// forgotten — that's its current, terminal state).
fn belief_echoes(
    buckets: &[DayBucket],
    kb_name: &str,
    docs: &[kb_core::storage::lance::DocSummary],
) -> Vec<BeliefOut> {
    let target: HashMap<NaiveDate, u8> = buckets.iter().map(|b| (b.date, b.months_ago)).collect();
    let superseded_by = superseded_by_map(docs);
    docs.iter()
        .filter(|d| is_memory_category(d.kb_category.as_deref()))
        .filter_map(|d| {
            let created = d.created_unix?;
            let date = date_of(created)?;
            let months_ago = *target.get(&date)?;
            let (status, superseded_by_id, superseded_at) =
                if d.kb_status.as_deref() == Some("forgotten") {
                    (BeliefStatus::Forgotten, None, None)
                } else if let Some(sup) = superseded_by.get(d.id.as_str()) {
                    (
                        BeliefStatus::Superseded,
                        Some(sup.id.clone()),
                        sup.created_unix,
                    )
                } else {
                    (BeliefStatus::Active, None, None)
                };
            Some(BeliefOut {
                id: d.id.clone(),
                kb: kb_name.to_string(),
                title: d.title.clone(),
                created,
                months_ago,
                status,
                superseded_by: superseded_by_id,
                superseded_at,
            })
        })
        .collect()
}

/// The `day_start` unix (see `day_bounds_unix`) of the furthest-back
/// bucket queried this response (ordinarily the 36-month anniversary) —
/// `None` only when every bucket was skipped (e.g. the leap-day case
/// producing zero whole-year hits).
fn oldest_bucket_start(buckets: &[DayBucket]) -> Option<i64> {
    buckets.iter().map(|b| day_bounds_unix(b.date).0).min()
}

/// CT-E6 / MI-W2.4c — EPOCH HONESTY, applied to the beliefs LANE as a
/// whole (not one memory's chain, unlike `kb memory log`'s per-id walk).
/// Same trigger, same wording family: the queried window reaches back
/// before this daemon ever became able to soft-forget rather than
/// hard-delete, so a memory hard-deleted in that older window left no
/// trace this lane — or `kb memory census`, or anywhere else — can show.
/// Gated on `has_memory_docs`: a kb with no memory-category docs at all
/// isn't memory-scoped, so there is nothing for the caveat to qualify.
fn tombstone_caveat_if_needed(
    buckets: &[DayBucket],
    has_memory_docs: bool,
    era_started_unix: i64,
) -> Option<String> {
    if !has_memory_docs {
        return None;
    }
    let oldest = oldest_bucket_start(buckets)?;
    (oldest < era_started_unix).then(|| {
        format!(
            "this anniversary window reaches back to {oldest} unix, before {era_started_unix} unix \
             — the moment this daemon became able to soft-forget (MI-W2.3) instead of hard-deleting. \
             A memory hard-deleted before {era_started_unix} left no trace and cannot appear in this \
             lane, `kb memory census`, or anywhere else."
        )
    })
}

fn kind_rank(k: EchoKind) -> u8 {
    match k {
        EchoKind::Created => 0,
        EchoKind::Worked => 1,
        EchoKind::Read => 2,
    }
}

fn tie_id(e: &EchoOut) -> &str {
    e.artifact_id
        .as_deref()
        .or(e.session_id.as_deref())
        .unwrap_or("")
}

/// Sort `months_ago` asc, then kind (created < worked < read), then id asc —
/// a total, deterministic order — and truncate to `limit`.
fn finalize(mut items: Vec<EchoOut>, limit: usize) -> Vec<EchoOut> {
    items.sort_by(|a, b| {
        a.months_ago
            .cmp(&b.months_ago)
            .then_with(|| kind_rank(a.kind).cmp(&kind_rank(b.kind)))
            .then_with(|| tie_id(a).cmp(tie_id(b)))
    });
    items.truncate(limit);
    items
}

#[cfg(test)]
mod tests {
    use super::*;

    fn session(id: &str, sid: &str, started_at: i64, edited: u32) -> SessionRow {
        SessionRow {
            artifact_id: id.into(),
            session_id: sid.into(),
            started_at,
            ended_at: started_at + 60,
            files_edited_count: edited,
            cwd: Some("/home/user/project/kb".into()),
            title: Some("do the thing".into()),
            ..SessionRow::default()
        }
    }

    #[test]
    fn resolve_limit_defaults_and_clamps() {
        assert_eq!(resolve_limit(None), 4);
        assert_eq!(resolve_limit(Some(0)), 1);
        assert_eq!(resolve_limit(Some(999)), 12);
        assert_eq!(resolve_limit(Some(7)), 7);
    }

    /// Whole-year policy: 29 Feb "today" skips every whole-year anniversary
    /// landing in a non-leap target year (2024 → 2023/2022/2021, all
    /// non-leap) rather than drifting to 28 Feb / 1 Mar. The 6-month
    /// half-anniversary is unaffected (Aug 29 exists).
    #[test]
    fn leap_day_anniversary_skips_non_leap_years_but_keeps_half_year() {
        let today = NaiveDate::from_ymd_opt(2024, 2, 29).unwrap();
        let buckets = anniversary_buckets(today);
        let months: Vec<u8> = buckets.iter().map(|b| b.months_ago).collect();
        assert_eq!(months, vec![6]);
    }

    /// Half-year policy: a day-of-month that doesn't exist 6 months back
    /// (31 Aug → "31 Feb") is skipped, never clamped to the 28th.
    #[test]
    fn half_anniversary_skipped_when_not_trivially_clean() {
        let today = NaiveDate::from_ymd_opt(2026, 8, 31).unwrap();
        let buckets = anniversary_buckets(today);
        assert!(buckets.iter().all(|b| b.months_ago != 6));
        // The whole-year anniversaries (31 Aug exists in every year) are
        // untouched by the half-year skip.
        assert_eq!(buckets.len(), 3);
    }

    #[test]
    fn ordinary_today_produces_all_four_buckets_in_ascending_distance() {
        let today = NaiveDate::from_ymd_opt(2026, 7, 21).unwrap();
        let buckets = anniversary_buckets(today);
        let months: Vec<u8> = buckets.iter().map(|b| b.months_ago).collect();
        assert_eq!(months, vec![6, 12, 24, 36]);
        assert_eq!(
            buckets[1].date,
            NaiveDate::from_ymd_opt(2025, 7, 21).unwrap()
        );
    }

    #[test]
    fn deterministic_same_inputs_same_output() {
        let mk = || {
            vec![
                EchoOut {
                    kind: EchoKind::Read,
                    months_ago: 12,
                    artifact_id: Some("bbb".into()),
                    source_relative: Some("b.html".into()),
                    title: "B".into(),
                    detail: "d".into(),
                    session_id: None,
                    started_at: None,
                },
                EchoOut {
                    kind: EchoKind::Created,
                    months_ago: 12,
                    artifact_id: Some("aaa".into()),
                    source_relative: Some("a.html".into()),
                    title: "A".into(),
                    detail: "d".into(),
                    session_id: None,
                    started_at: None,
                },
            ]
        };
        let flat = |v: &[EchoOut]| {
            v.iter()
                .map(|e| (e.kind, e.artifact_id.clone()))
                .collect::<Vec<_>>()
        };
        let a = finalize(mk(), 10);
        let b = finalize(mk(), 10);
        assert_eq!(flat(&a), flat(&b));
    }

    #[test]
    fn caps_truncate_after_sort() {
        let items: Vec<EchoOut> = (0..5)
            .map(|i| EchoOut {
                kind: EchoKind::Created,
                months_ago: 12,
                artifact_id: Some(format!("{i:03}")),
                source_relative: Some("x.html".into()),
                title: "T".into(),
                detail: "d".into(),
                session_id: None,
                started_at: None,
            })
            .collect();
        let out = finalize(items, 2);
        assert_eq!(out.len(), 2);
        assert_eq!(out[0].artifact_id.as_deref(), Some("000"));
        assert_eq!(out[1].artifact_id.as_deref(), Some("001"));
    }

    #[test]
    fn sort_orders_by_distance_then_kind_then_id() {
        let mk = |kind, months, id: &str| EchoOut {
            kind,
            months_ago: months,
            artifact_id: Some(id.into()),
            source_relative: Some("x.html".into()),
            title: "T".into(),
            detail: "d".into(),
            session_id: None,
            started_at: None,
        };
        let items = vec![
            mk(EchoKind::Read, 12, "zzz"),
            mk(EchoKind::Created, 24, "aaa"),
            mk(EchoKind::Worked, 12, "mmm"),
            mk(EchoKind::Created, 12, "yyy"),
            mk(EchoKind::Created, 12, "aaa"),
        ];
        let out = finalize(items, 10);
        let ids: Vec<&str> = out
            .iter()
            .map(|e| e.artifact_id.as_deref().unwrap())
            .collect();
        assert_eq!(ids, ["aaa", "yyy", "mmm", "zzz", "aaa"]);
        assert_eq!(out[4].months_ago, 24);
    }

    // invariant:11 — one session_id accrues many `sessions` rows (a capture
    // per Stop); every per-session read must scope to the newest capture.
    #[test]
    fn collapse_newest_capture_keeps_only_the_latest_row_per_session_id() {
        let rows = vec![
            session("cap-early", "s1", 100, 1),
            session("cap-late", "s1", 200, 3),
            session("other", "s2", 150, 2),
        ];
        let out = collapse_newest_capture(rows);
        assert_eq!(out.len(), 2);
        let s1 = out.iter().find(|r| r.session_id == "s1").unwrap();
        assert_eq!(s1.artifact_id, "cap-late");
        assert_eq!(s1.files_edited_count, 3);
    }

    // invariant:11 — a same-`started_at` tie breaks on the SMALLER
    // artifact_id (mirrors `sessions_list`'s `ORDER BY started_at DESC,
    // artifact_id ASC LIMIT 1`).
    #[test]
    fn collapse_newest_capture_tiebreaks_equal_started_at_on_smaller_artifact_id() {
        let rows = vec![
            session("zzz999", "s1", 100, 1),
            session("aaa111", "s1", 100, 9),
        ];
        let out = collapse_newest_capture(rows);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].artifact_id, "aaa111");
    }

    #[test]
    fn worked_echoes_builds_beat_from_title_cwd_and_edit_count() {
        let today = NaiveDate::from_ymd_opt(2026, 7, 21).unwrap();
        let buckets = anniversary_buckets(today);
        let last_year = day_bounds_unix(NaiveDate::from_ymd_opt(2025, 7, 21).unwrap()).0 + 3600;
        let rows = vec![session("art1", "s1", last_year, 5)];
        let out = worked_echoes(&buckets, rows);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].kind, EchoKind::Worked);
        assert_eq!(out[0].months_ago, 12);
        assert_eq!(out[0].title, "do the thing");
        assert_eq!(out[0].session_id.as_deref(), Some("s1"));
        assert_eq!(out[0].detail, "kb · 5 files edited · last year");
    }

    #[test]
    fn created_and_read_echoes_skip_docs_missing_from_meta_or_dates() {
        let today = NaiveDate::from_ymd_opt(2026, 7, 21).unwrap();
        let buckets = anniversary_buckets(today);
        let mut meta = HashMap::new();
        let created_ts = day_bounds_unix(NaiveDate::from_ymd_opt(2024, 7, 21).unwrap()).0 + 100;
        meta.insert(
            "art-created".to_string(),
            DocMeta {
                title: "Old Doc".into(),
                source_relative: "old.html".into(),
                created_unix: Some(created_ts),
            },
        );
        meta.insert(
            "art-no-date".to_string(),
            DocMeta {
                title: "No Date".into(),
                source_relative: "nd.html".into(),
                created_unix: None,
            },
        );
        let created = created_echoes(&buckets, &meta);
        assert_eq!(created.len(), 1);
        assert_eq!(created[0].months_ago, 24);
        assert_eq!(created[0].detail, "created this day 2 years ago");

        let mut counts = HashMap::new();
        counts.insert("art-created".to_string(), 3u32);
        counts.insert("art-unknown".to_string(), 1u32);
        let read = read_echoes_for_bucket(&buckets[1], &counts, &meta);
        assert_eq!(read.len(), 1);
        assert_eq!(read[0].detail, "read 3 times this day last year");
    }

    // === CT-E6 — the beliefs lane ==========================================

    fn memory_doc(
        id: &str,
        category: &str,
        created_unix: i64,
        status: Option<&str>,
        supersedes: Option<&str>,
    ) -> kb_core::storage::lance::DocSummary {
        kb_core::storage::lance::DocSummary {
            id: id.into(),
            title: format!("memory {id}"),
            kb_category: Some(category.into()),
            created_unix: Some(created_unix),
            kb_status: status.map(str::to_string),
            kb_supersedes: supersedes.map(str::to_string),
            ..Default::default()
        }
    }

    #[test]
    fn belief_echoes_reports_active_memory_on_anniversary() {
        let today = NaiveDate::from_ymd_opt(2026, 7, 21).unwrap();
        let buckets = anniversary_buckets(today);
        let last_year = day_bounds_unix(NaiveDate::from_ymd_opt(2025, 7, 21).unwrap()).0 + 100;
        let docs = vec![memory_doc("mem1", "memory-user", last_year, None, None)];
        let out = belief_echoes(&buckets, "kb1", &docs);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].kb, "kb1");
        assert_eq!(out[0].months_ago, 12);
        assert_eq!(out[0].status, BeliefStatus::Active);
        assert!(out[0].superseded_by.is_none());
    }

    #[test]
    fn belief_echoes_reports_superseded_with_successor_id_and_date() {
        let today = NaiveDate::from_ymd_opt(2026, 7, 21).unwrap();
        let buckets = anniversary_buckets(today);
        let last_year = day_bounds_unix(NaiveDate::from_ymd_opt(2025, 7, 21).unwrap()).0 + 100;
        let successor_created = last_year + 3600;
        let docs = vec![
            memory_doc("old-mem", "memory-user", last_year, None, None),
            memory_doc(
                "new-mem",
                "memory-user",
                successor_created,
                None,
                Some("old-mem"),
            ),
        ];
        let out = belief_echoes(&buckets, "kb1", &docs);
        let old = out.iter().find(|b| b.id == "old-mem").unwrap();
        assert_eq!(old.status, BeliefStatus::Superseded);
        assert_eq!(old.superseded_by.as_deref(), Some("new-mem"));
        assert_eq!(old.superseded_at, Some(successor_created));
    }

    #[test]
    fn belief_echoes_forgotten_status_wins_over_superseded() {
        // A tombstoned memory that ALSO has a successor still reports its
        // current terminal state (forgotten), not superseded.
        let today = NaiveDate::from_ymd_opt(2026, 7, 21).unwrap();
        let buckets = anniversary_buckets(today);
        let last_year = day_bounds_unix(NaiveDate::from_ymd_opt(2025, 7, 21).unwrap()).0 + 100;
        let docs = vec![
            memory_doc("old-mem", "memory-user", last_year, Some("forgotten"), None),
            memory_doc(
                "new-mem",
                "memory-user",
                last_year + 3600,
                None,
                Some("old-mem"),
            ),
        ];
        let out = belief_echoes(&buckets, "kb1", &docs);
        let old = out.iter().find(|b| b.id == "old-mem").unwrap();
        assert_eq!(old.status, BeliefStatus::Forgotten);
        assert!(old.superseded_by.is_none());
    }

    #[test]
    fn belief_echoes_skips_non_memory_category_and_session_transcripts() {
        let today = NaiveDate::from_ymd_opt(2026, 7, 21).unwrap();
        let buckets = anniversary_buckets(today);
        let last_year = day_bounds_unix(NaiveDate::from_ymd_opt(2025, 7, 21).unwrap()).0 + 100;
        let docs = vec![
            memory_doc("proj-doc", "project", last_year, None, None),
            memory_doc("transcript", "memory-session", last_year, None, None),
        ];
        assert!(belief_echoes(&buckets, "kb1", &docs).is_empty());
    }

    #[test]
    fn tombstone_caveat_absent_when_kb_has_no_memory_docs() {
        let today = NaiveDate::from_ymd_opt(2026, 7, 21).unwrap();
        let buckets = anniversary_buckets(today);
        // A window that DOES reach before an era far in the future would
        // otherwise trip the caveat — `has_memory_docs = false` must still
        // suppress it.
        let far_future_era = chrono::Utc::now().timestamp() + 999_999_999;
        assert!(tombstone_caveat_if_needed(&buckets, false, far_future_era).is_none());
    }

    #[test]
    fn tombstone_caveat_present_when_window_predates_era() {
        let today = NaiveDate::from_ymd_opt(2026, 7, 21).unwrap();
        let buckets = anniversary_buckets(today);
        let era_started = oldest_bucket_start(&buckets).unwrap() + 1;
        let caveat = tombstone_caveat_if_needed(&buckets, true, era_started).unwrap();
        assert!(caveat.contains("soft-forget"));
        assert!(caveat.contains(&era_started.to_string()));
    }

    #[test]
    fn tombstone_caveat_absent_when_window_postdates_era() {
        let today = NaiveDate::from_ymd_opt(2026, 7, 21).unwrap();
        let buckets = anniversary_buckets(today);
        let era_started = oldest_bucket_start(&buckets).unwrap() - 1;
        assert!(tombstone_caveat_if_needed(&buckets, true, era_started).is_none());
    }
}
