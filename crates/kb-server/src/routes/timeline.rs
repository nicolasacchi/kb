//! `GET /api/kb/{kb}/timeline` — four day-bucketed lanes (created / read /
//! session / comment) sharing ONE UTC-day x-axis, the foundation of the
//! multi-facet reflection canvas. A pure, read-only derived view exactly
//! like [`crate::routes::echoes`] and [`crate::routes::history::get_calendar`]
//! before it: `resolve_kb` → a handful of read-only storage calls → pure
//! bucketing → `Json`. No writes, no SSE emission, no new `StorageMsg` — every
//! call below already exists on [`kb_core::storage::actor::StorageHandle`].
//!
//! **The four lanes, and why each reads what it reads:**
//!  - `created` — [`crate::routes::docs::gallery_snapshot`]'s row set,
//!    bucketed on `mtime_unix` (never `indexed_at_unix`, which is
//!    `unix_now()` at index time and would stamp the whole corpus with
//!    today after one `kb reindex` — the same axis invariant #35's
//!    `?from`/`?to` gallery filter already uses).
//!  - `read` — `Db::history_counts_by_day` (W2.10, `kind='open'`) for exact
//!    per-day counts, plus `history_opens_in_window` for the (capped)
//!    resolved artifact-id set — two existing calls, not a new one.
//!  - `session` — `sessions_list(u32::MAX, …)`, whose WHERE clause already
//!    collapses multi-capture sessions to the newest capture per
//!    `session_id` (invariant #11); [`collapse_newest_capture`] re-applies
//!    the same fold purely (a local twin of `echoes::collapse_newest_capture`)
//!    so the policy stays unit-testable without a DB. Reads EMPTY on a
//!    non-sessions corpus — that's correct, not a bug, and the lane's
//!    `label` says so rather than hiding it.
//!  - `comment` — `Db::history_counts_by_day` (`kind='comment'`) for counts
//!    plus `history_comments_in_window` for ids — the daemon's own recorded
//!    `history` rows (written at `routes::comments::create`), NOT a per-request
//!    walk of every `.review/<id>.json` (that cost shape is `routes::inbox`'s
//!    `collect_open`, wrong for a per-request read here). kb-comments/1 has
//!    no `resolved_at`, so only *raised* is honest — the label says so.
//!
//! **Synchronized lanes**: all four lanes share one continuous UTC-day axis
//! ([`day_range`]) spanning `[from, to]`, zero-filled — so the SPA can later
//! render the four tracks on one shared timeline without per-lane gap logic.
//!
//! **`ids` is a pivot handle, not a display list**: each lane also returns
//! the resolved artifact-id set for the window (capped at
//! [`TIMELINE_ID_CAP`], the same 5000 [`crate::routes::search`]'s
//! `READ_WINDOW_LIMIT` uses), so the SPA can later route straight into the
//! gallery through the already-shipped `?ids=` filter (invariant #35).
//! `truncated` reflects whether the underlying bounded query hit its cap —
//! for `read`/`comment` that's the row cap (dedup only ever shrinks further,
//! never grows it back over cap); for `created`/`session` it's the id-list
//! length itself, since those two lanes read an unbounded full scan.

use crate::middleware::error_to_problem_json;
use crate::state::KbHandles;
use axum::{
    body::Body,
    extract::{Path, Query, State},
    http::Response,
    response::IntoResponse,
    Json,
};
use kb_core::docs_query::DocRow;
use kb_core::storage::sqlite::{DayKindCount, HistoryRow, SessionRow};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::Arc;

/// Default lookback when `from` is omitted: the last 365 days up to `to`.
/// Matches `history::get_calendar`'s `DEFAULT_CALENDAR_SPAN_SECS` — the same
/// UTC-day contract, so the two endpoints agree on "how far back is normal".
const DEFAULT_SPAN_SECS: i64 = 365 * 24 * 60 * 60;
/// Requested span cap — a shade over a year of slack, matching
/// `history::get_calendar`'s `MAX_CALENDAR_SPAN_SECS`. Keeps the day-range
/// loop and every bucketing pass bounded.
const MAX_SPAN_SECS: i64 = 400 * 24 * 60 * 60;
/// Per-lane cap on the resolved artifact-id set — matches
/// `routes::search::READ_WINDOW_LIMIT`, the existing precedent for "how many
/// ids is a window join allowed to resolve before it must admit truncation".
const TIMELINE_ID_CAP: usize = 5000;

#[cfg_attr(feature = "ts-export", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Serialize, Clone, Copy, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum TimelineTrack {
    Created,
    Read,
    Session,
    Comment,
}

/// One UTC calendar day's count for a single lane.
#[cfg_attr(feature = "ts-export", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Clone, Serialize)]
pub struct TimelineDay {
    pub day: String,
    pub count: i64,
}

#[cfg_attr(feature = "ts-export", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Serialize)]
pub struct TimelineLane {
    pub track: TimelineTrack,
    /// A deterministic, honest sentence about what this lane's numbers mean
    /// (and, for `session`, that an empty lane on a non-sessions corpus is
    /// expected — not a fault).
    pub label: String,
    /// One entry per day in `[from, to]`, zero-filled, same length/order as
    /// every other lane's `days` (the "synchronized" contract).
    pub days: Vec<TimelineDay>,
    /// Sum of `days[..].count` — exact (never affected by the `ids` cap).
    pub total: i64,
    /// Resolved artifact ids for the window, capped at [`TIMELINE_ID_CAP`].
    pub ids: Vec<String>,
    pub truncated: bool,
}

#[cfg_attr(
    feature = "ts-export",
    derive(ts_rs::TS),
    ts(export, rename = "TimelineResponse")
)]
#[derive(Debug, Serialize)]
pub struct TimelineResponse {
    pub from: i64,
    pub to: i64,
    pub lanes: Vec<TimelineLane>,
}

#[derive(Debug, Deserialize, Default)]
pub struct TimelineParams {
    /// Unix seconds OR `YYYY-MM-DD` (UTC midnight). Defaults to `to - 365d`.
    pub from: Option<String>,
    /// Unix seconds OR `YYYY-MM-DD` (UTC midnight). Defaults to now.
    pub to: Option<String>,
}

pub async fn list(
    State(state): State<Arc<KbHandles>>,
    Path(kb): Path<String>,
    Query(params): Query<TimelineParams>,
) -> Response<Body> {
    let (_kb_name, ctx) = match crate::routes::resolve_kb(&state, &kb) {
        Ok(v) => v,
        Err(resp) => return resp,
    };

    let now = chrono::Utc::now().timestamp();
    let to = match params.to.as_deref().map(parse_bound).transpose() {
        Ok(v) => v.unwrap_or(now),
        Err(e) => return error_to_problem_json(&e),
    };
    let from = match params.from.as_deref().map(parse_bound).transpose() {
        Ok(v) => v.unwrap_or(to - DEFAULT_SPAN_SECS),
        Err(e) => return error_to_problem_json(&e),
    };
    if to < from {
        return error_to_problem_json(&kb_core::Error::BadRequest("to must be >= from".into()));
    }
    if to - from > MAX_SPAN_SECS {
        return error_to_problem_json(&kb_core::Error::BadRequest(format!(
            "requested range exceeds the {MAX_SPAN_SECS}s cap"
        )));
    }

    let days = day_range(from, to);

    // created — one shared scan with facets/gallery (#15's memo).
    let (rows, _edge_counts) = match crate::routes::docs::gallery_snapshot(ctx).await {
        Ok(v) => v,
        Err(e) => return error_to_problem_json(&e),
    };
    let (created_counts, mut created_ids) = bucket_docs_by_day(&rows, from, to);
    let created_truncated = created_ids.len() > TIMELINE_ID_CAP;
    created_ids.truncate(TIMELINE_ID_CAP);

    // session — full scan, #11 collapse re-applied purely.
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
    let sessions = collapse_newest_capture(sessions);
    let (session_counts, mut session_ids) = bucket_sessions_by_day(&sessions, from, to);
    let session_truncated = session_ids.len() > TIMELINE_ID_CAP;
    session_ids.truncate(TIMELINE_ID_CAP);

    // read + comment — exact counts from ONE `GROUP BY` scan (W2.10),
    // capped id sets from the existing window-bounded reads.
    let day_kind_rows = match ctx.storage.history_counts_by_day(from, to).await {
        Ok(r) => r,
        Err(e) => return error_to_problem_json(&e),
    };
    let (read_counts, comment_counts) = pivot_day_kind_counts(&day_kind_rows);

    let open_rows = match ctx
        .storage
        .history_opens_in_window(from, to, TIMELINE_ID_CAP as u32)
        .await
    {
        Ok(r) => r,
        Err(e) => return error_to_problem_json(&e),
    };
    let read_truncated = open_rows.len() == TIMELINE_ID_CAP;
    let read_ids = dedup_ids(&open_rows);

    let comment_rows = match ctx
        .storage
        .history_comments_in_window(from, to, TIMELINE_ID_CAP as u32)
        .await
    {
        Ok(r) => r,
        Err(e) => return error_to_problem_json(&e),
    };
    let comment_truncated = comment_rows.len() == TIMELINE_ID_CAP;
    let comment_ids = dedup_ids(&comment_rows);

    let lanes = vec![
        build_lane(
            TimelineTrack::Created,
            "artifacts created, by filesystem mtime",
            &days,
            &created_counts,
            created_ids,
            created_truncated,
        ),
        build_lane(
            TimelineTrack::Read,
            "artifact opens recorded by this daemon",
            &days,
            &read_counts,
            read_ids,
            read_truncated,
        ),
        build_lane(
            TimelineTrack::Session,
            "work sessions captured on this daemon (empty outside a sessions corpus)",
            &days,
            &session_counts,
            session_ids,
            session_truncated,
        ),
        build_lane(
            TimelineTrack::Comment,
            "comments raised, as recorded by this daemon (kb-comments/1 has no resolved_at)",
            &days,
            &comment_counts,
            comment_ids,
            comment_truncated,
        ),
    ];

    Json(TimelineResponse { from, to, lanes }).into_response()
}

/// Parse a `from`/`to` bound: either a bare unix-seconds integer or a
/// `YYYY-MM-DD` calendar date (UTC midnight) — the same grammar
/// `kb-cli`'s (private) `commands::search::parse_time_bound` uses for
/// `--read-from`/`--read-to`, duplicated here rather than exposed
/// cross-crate (small pure helper, matching this route family's existing
/// locality convention — e.g. `echoes::cwd_basename`).
fn parse_bound(s: &str) -> Result<i64, kb_core::Error> {
    if let Ok(secs) = s.parse::<i64>() {
        return Ok(secs);
    }
    let date = chrono::NaiveDate::parse_from_str(s, "%Y-%m-%d").map_err(|e| {
        kb_core::Error::BadRequest(format!(
            "invalid from/to {s:?}: expected unix seconds or YYYY-MM-DD ({e})"
        ))
    })?;
    let dt = date
        .and_hms_opt(0, 0, 0)
        .ok_or_else(|| kb_core::Error::BadRequest(format!("invalid date {s:?}")))?;
    Ok(dt.and_utc().timestamp())
}

fn date_of(unix: i64) -> Option<chrono::NaiveDate> {
    chrono::DateTime::from_timestamp(unix, 0).map(|dt| dt.date_naive())
}

/// Every UTC calendar day in `[from_unix, to_unix]`, ascending, as
/// `YYYY-MM-DD` — the shared x-axis every lane zero-fills onto. Bounded by
/// the caller's `MAX_SPAN_SECS` check (at most ~400 entries).
fn day_range(from_unix: i64, to_unix: i64) -> Vec<String> {
    let (Some(start), Some(end)) = (date_of(from_unix), date_of(to_unix)) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    let mut d = start;
    while d <= end {
        out.push(d.format("%Y-%m-%d").to_string());
        d = match d.succ_opt() {
            Some(next) => next,
            None => break,
        };
    }
    out
}

/// #11 — collapse multi-capture sessions to the NEWEST capture per
/// `session_id` (`started_at` desc, `artifact_id` asc tiebreak — the same
/// rule `sessions_list`'s own SQL enforces). A local twin of
/// `echoes::collapse_newest_capture`: duplicated rather than exposed
/// cross-module (small pure helper, matching this route family's locality
/// convention), so the policy is unit-testable without a DB here too.
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

/// `created` lane — per-day counts + the sorted (deterministic), unbounded
/// id list for docs whose `mtime_unix` falls in `[from, to]`. The caller
/// applies the [`TIMELINE_ID_CAP`] truncation.
fn bucket_docs_by_day(rows: &[DocRow], from: i64, to: i64) -> (HashMap<String, i64>, Vec<String>) {
    let mut counts: HashMap<String, i64> = HashMap::new();
    let mut ids: Vec<String> = Vec::new();
    for row in rows {
        let Some(mtime) = row.doc.mtime_unix else {
            continue;
        };
        if mtime < from || mtime > to {
            continue;
        }
        let Some(date) = date_of(mtime) else {
            continue;
        };
        *counts
            .entry(date.format("%Y-%m-%d").to_string())
            .or_insert(0) += 1;
        ids.push(row.doc.id.clone());
    }
    ids.sort();
    (counts, ids)
}

/// `session` lane — per-day counts + the sorted (deterministic), unbounded
/// id list for sessions (already collapsed to newest-capture) whose
/// `started_at` falls in `[from, to]`.
fn bucket_sessions_by_day(
    rows: &[SessionRow],
    from: i64,
    to: i64,
) -> (HashMap<String, i64>, Vec<String>) {
    let mut counts: HashMap<String, i64> = HashMap::new();
    let mut ids: Vec<String> = Vec::new();
    for row in rows {
        if row.started_at < from || row.started_at > to {
            continue;
        }
        let Some(date) = date_of(row.started_at) else {
            continue;
        };
        *counts
            .entry(date.format("%Y-%m-%d").to_string())
            .or_insert(0) += 1;
        ids.push(row.artifact_id.clone());
    }
    ids.sort();
    (counts, ids)
}

/// Pivot `Db::history_counts_by_day`'s per-`(day, kind)` rows into two maps
/// — `(read_counts, comment_counts)` — reading `kind='open'` as the `read`
/// lane's exact per-day counts and `kind='comment'` as the `comment` lane's.
/// `kind='search'` rows (also present in the same table) are not a timeline
/// lane and are dropped here, same as `history::pivot_calendar_days` drops
/// an unrecognised kind.
fn pivot_day_kind_counts(rows: &[DayKindCount]) -> (HashMap<String, i64>, HashMap<String, i64>) {
    let mut read_counts: HashMap<String, i64> = HashMap::new();
    let mut comment_counts: HashMap<String, i64> = HashMap::new();
    for r in rows {
        match r.kind.as_str() {
            "open" => {
                *read_counts.entry(r.day.clone()).or_insert(0) += r.count;
            }
            "comment" => {
                *comment_counts.entry(r.day.clone()).or_insert(0) += r.count;
            }
            _ => {}
        }
    }
    (read_counts, comment_counts)
}

/// Dedup `HistoryRow::artifact_id` while preserving the rows' own
/// (deterministic, `started_at DESC, id DESC`) order — first-seen wins, so
/// the newest visit/comment for an artifact decides its position.
fn dedup_ids(rows: &[HistoryRow]) -> Vec<String> {
    let mut seen: std::collections::HashSet<&str> = std::collections::HashSet::new();
    let mut out = Vec::new();
    for r in rows {
        if let Some(id) = r.artifact_id.as_deref() {
            if seen.insert(id) {
                out.push(id.to_string());
            }
        }
    }
    out
}

/// Zero-fill `counts` onto the shared `days` axis and assemble one
/// [`TimelineLane`]. `total` sums the (unfilled, exact) `counts` map
/// directly rather than the zero-filled series, so it's unaffected by the
/// caller's day-range slicing rounding.
fn build_lane(
    track: TimelineTrack,
    label: &str,
    days: &[String],
    counts: &HashMap<String, i64>,
    ids: Vec<String>,
    truncated: bool,
) -> TimelineLane {
    let total: i64 = counts.values().sum();
    let days = days
        .iter()
        .map(|d| TimelineDay {
            day: d.clone(),
            count: *counts.get(d).unwrap_or(&0),
        })
        .collect();
    TimelineLane {
        track,
        label: label.to_string(),
        days,
        total,
        ids,
        truncated,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn doc_row(id: &str, mtime: i64) -> DocRow {
        DocRow {
            doc: kb_core::storage::lance::DocSummary {
                id: id.to_string(),
                mtime_unix: Some(mtime),
                ..Default::default()
            },
            folder: String::new(),
        }
    }

    fn session(id: &str, sid: &str, started_at: i64) -> SessionRow {
        SessionRow {
            artifact_id: id.into(),
            session_id: sid.into(),
            started_at,
            ended_at: started_at + 60,
            ..SessionRow::default()
        }
    }

    #[test]
    fn parse_bound_accepts_unix_seconds() {
        assert_eq!(parse_bound("1700000000").unwrap(), 1_700_000_000);
        assert_eq!(parse_bound("-5").unwrap(), -5);
    }

    #[test]
    fn parse_bound_accepts_calendar_date_at_utc_midnight() {
        assert_eq!(parse_bound("2024-01-01").unwrap(), 1_704_067_200);
    }

    #[test]
    fn parse_bound_rejects_garbage() {
        assert!(parse_bound("not-a-date").is_err());
        assert!(parse_bound("2024-13-99").is_err());
    }

    #[test]
    fn day_range_is_inclusive_ascending() {
        let from = chrono::NaiveDate::from_ymd_opt(2026, 7, 1)
            .unwrap()
            .and_hms_opt(0, 0, 0)
            .unwrap()
            .and_utc()
            .timestamp();
        let to = chrono::NaiveDate::from_ymd_opt(2026, 7, 3)
            .unwrap()
            .and_hms_opt(12, 0, 0)
            .unwrap()
            .and_utc()
            .timestamp();
        let days = day_range(from, to);
        assert_eq!(days, vec!["2026-07-01", "2026-07-02", "2026-07-03"]);
    }

    #[test]
    fn day_range_single_day_when_from_equals_to() {
        let ts = 1_700_000_000;
        assert_eq!(day_range(ts, ts).len(), 1);
    }

    // invariant:11 — one session_id accrues many `sessions` rows (a capture
    // per Stop); every per-session read must scope to the newest capture.
    #[test]
    fn collapse_newest_capture_keeps_only_the_latest_row_per_session_id() {
        let rows = vec![
            session("cap-early", "s1", 100),
            session("cap-late", "s1", 200),
            session("other", "s2", 150),
        ];
        let out = collapse_newest_capture(rows);
        assert_eq!(out.len(), 2);
        let s1 = out.iter().find(|r| r.session_id == "s1").unwrap();
        assert_eq!(s1.artifact_id, "cap-late");
    }

    #[test]
    fn collapse_newest_capture_tiebreaks_equal_started_at_on_smaller_artifact_id() {
        let rows = vec![session("zzz999", "s1", 100), session("aaa111", "s1", 100)];
        let out = collapse_newest_capture(rows);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].artifact_id, "aaa111");
    }

    #[test]
    fn bucket_docs_by_day_skips_missing_mtime_and_out_of_window() {
        let rows = vec![
            doc_row("in-window", 1_700_000_000),
            doc_row("out-of-window", 1_600_000_000),
            DocRow {
                doc: kb_core::storage::lance::DocSummary {
                    id: "no-mtime".into(),
                    mtime_unix: None,
                    ..Default::default()
                },
                folder: String::new(),
            },
        ];
        let (counts, ids) = bucket_docs_by_day(&rows, 1_650_000_000, 1_750_000_000);
        assert_eq!(ids, vec!["in-window".to_string()]);
        assert_eq!(counts.values().sum::<i64>(), 1);
    }

    #[test]
    fn bucket_sessions_by_day_filters_by_started_at_window() {
        let rows = vec![
            session("a", "s1", 1_700_000_000),
            session("b", "s2", 1_600_000_000),
        ];
        let (counts, ids) = bucket_sessions_by_day(&rows, 1_650_000_000, 1_750_000_000);
        assert_eq!(ids, vec!["a".to_string()]);
        assert_eq!(counts.values().sum::<i64>(), 1);
    }

    #[test]
    fn pivot_day_kind_counts_splits_open_and_comment_drops_search() {
        let rows = vec![
            DayKindCount {
                day: "2026-07-01".into(),
                kind: "open".into(),
                count: 3,
            },
            DayKindCount {
                day: "2026-07-01".into(),
                kind: "comment".into(),
                count: 1,
            },
            DayKindCount {
                day: "2026-07-01".into(),
                kind: "search".into(),
                count: 9,
            },
        ];
        let (read_counts, comment_counts) = pivot_day_kind_counts(&rows);
        assert_eq!(read_counts.get("2026-07-01"), Some(&3));
        assert_eq!(comment_counts.get("2026-07-01"), Some(&1));
    }

    fn history_row(artifact_id: &str) -> HistoryRow {
        HistoryRow {
            id: 1,
            kind: "open".into(),
            artifact_id: Some(artifact_id.into()),
            query: None,
            comment_id: None,
            scroll_y: 0,
            scroll_max: 0,
            scroll_y_max: 0,
            started_at_unix: 0,
            updated_at_unix: 0,
            source: None,
            user: "operator".into(),
        }
    }

    #[test]
    fn dedup_ids_preserves_first_seen_order() {
        let rows = vec![history_row("aaa"), history_row("bbb"), history_row("aaa")];
        assert_eq!(dedup_ids(&rows), vec!["aaa".to_string(), "bbb".to_string()]);
    }

    #[test]
    fn build_lane_zero_fills_missing_days_and_sums_total_from_counts() {
        let days = vec![
            "2026-07-01".to_string(),
            "2026-07-02".to_string(),
            "2026-07-03".to_string(),
        ];
        let mut counts = HashMap::new();
        counts.insert("2026-07-01".to_string(), 2);
        counts.insert("2026-07-03".to_string(), 5);
        let lane = build_lane(
            TimelineTrack::Read,
            "label",
            &days,
            &counts,
            vec!["a".into()],
            false,
        );
        assert_eq!(lane.days.len(), 3);
        assert_eq!(lane.days[1].count, 0);
        assert_eq!(lane.total, 7);
    }
}
