//! v0.6+ H2 — per-user activity log endpoints.
//!
//! `POST /api/kb/{kb}/history/open` — register an artifact-view visit
//!   (30-minute gap rule, see `kb_core::storage::Db::history_record_open`).
//!   Returns `{ visit_id, scroll_y }`. The SPA passes `scroll_y` to the
//!   iframe runtime as the resume target.
//!
//! `POST /api/kb/{kb}/history/scroll` — UPDATE scroll position on an
//!   existing open visit. Body: `{ visit_id, scroll_y, scroll_max }`.
//!   Returns 204 No Content. Stale or non-open visit_ids return 404.
//!   Called frequently (debounced 1s in the SPA); no SSE emit.
//!
//! `POST /api/kb/{kb}/history/reading` — RP-track per-section reading
//!   beacon. Body: `{ visit_id, artifact_id, sections[], active_ms,
//!   last_section }` carrying CUMULATIVE per-visit dwell/enters; the server
//!   max-merges so resent beacons / remount-restores can't double-count.
//!   204 on success, 404 for a stale visit, a no-op 204 when the kb has
//!   `reading_progress = false`. No SSE (like scroll). `POST …/history/open`
//!   echoes the prior reading state (seed-on-open) so the runtime resumes
//!   its counters within the 30-minute visit window.
//!
//! `POST /api/kb/{kb}/history/search` — record a search query with
//!   5-second dedup. Body: `{ query }`. The SPA calls this on Enter or
//!   click-result, not on every keystroke.
//!
//! `GET /api/kb/{kb}/history?limit=&before=&kind=` — newest-first list.
//!   `kind` filter accepts `open`/`search`/`comment`/`all`. Open and
//!   comment entries are enriched with the artifact title via lance.
//!
//! `GET /api/kb/{kb}/history/calendar?from=&to=` — W2.10: per-UTC-day event
//!   density (`{ days: [{ day, opens, searches, comments }] }`) for the
//!   gallery's activity calendar. One bounded `GROUP BY` over `history`;
//!   `from`/`to` default to a trailing 365-day window, capped at ~400 days.
//!
//! New SSE event `history.recorded` fires on actual inserts (not bumps,
//! not scroll updates). Payload carries `{ kb, kind, id, artifact_id?,
//! query?, comment_id? }`. The SPA gallery's history view subscribes to
//! refetch on it (same cooldown pattern as `artifact.indexed`).

use crate::middleware::error_to_problem_json;
use crate::state::KbHandles;
use axum::{
    body::Body,
    extract::{Extension, Path, Query, State},
    http::{Response, StatusCode},
    response::IntoResponse,
    Json,
};
use kb_core::storage::sqlite::SectionDwell;
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::sync::Arc;

use super::is_safe_id;

const DEFAULT_LIMIT: u32 = 200;
const MAX_LIMIT: u32 = 1000;

#[cfg_attr(
    feature = "ts-export",
    derive(ts_rs::TS),
    ts(export, rename = "HistoryOpenBody")
)]
#[derive(Debug, Deserialize)]
pub struct OpenBody {
    pub artifact_id: String,
    /// GC-B5 — who's opening: `"web"` (the SPA; also the default when the
    /// field is omitted, e.g. every pre-existing caller) or `"cli"` (`kb
    /// cat`/`kb get`/`kb read` recording an agent read through the daemon).
    /// Any other value is dropped to `None` rather than rejected — this is
    /// a soft telemetry hint, not something worth 400ing a visit over.
    #[serde(default)]
    #[cfg_attr(feature = "ts-export", ts(optional))]
    pub source: Option<String>,
}

#[cfg_attr(
    feature = "ts-export",
    derive(ts_rs::TS),
    ts(export, rename = "HistoryOpenResponse")
)]
#[derive(Debug, Serialize)]
pub struct OpenResponse {
    pub visit_id: i64,
    pub scroll_y: i64,
    /// RP-track seed-on-open (F6): the visit's prior reading state so the
    /// iframe runtime restores its cumulative dwell/active counters on a
    /// 30-min resume (the server max-merges, so the client value must never
    /// regress). `None` when reading capture is disabled for this kb.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts-export", ts(optional))]
    pub reading: Option<ReadingResumeState>,
}

/// Resume baseline echoed by `POST …/history/open` (RP-track). The runtime
/// seeds its in-memory accumulators from this once, then re-measures text /
/// words / pixels from the live DOM.
#[cfg_attr(feature = "ts-export", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Serialize)]
pub struct ReadingResumeState {
    pub active_ms: i64,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts-export", ts(optional))]
    pub last_section: Option<String>,
    pub sections: Vec<ReadingResumeSection>,
}

#[cfg_attr(feature = "ts-export", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Serialize)]
pub struct ReadingResumeSection {
    pub id: String,
    pub dwell_ms: i64,
    pub enters: i64,
}

pub async fn post_open(
    State(state): State<Arc<KbHandles>>,
    Extension(identity): Extension<crate::middleware::Identity>,
    Path(kb): Path<String>,
    Json(body): Json<OpenBody>,
) -> Response<Body> {
    let (kb_name, ctx) = match crate::routes::resolve_kb(&state, &kb) {
        Ok(v) => v,
        Err(resp) => return resp,
    };
    if !is_safe_id(&body.artifact_id) {
        return error_to_problem_json(&kb_core::Error::BadRequest(format!(
            "artifact_id {:?} contains illegal characters",
            body.artifact_id
        )));
    }
    let source = match body.source.as_deref() {
        Some("web") => Some("web".to_string()),
        Some("cli") => Some("cli".to_string()),
        _ => None,
    };
    let now_unix = chrono::Utc::now().timestamp();
    let user = identity.user.clone();
    let result = match ctx
        .storage
        .history_record_open(body.artifact_id.clone(), now_unix, source, user.clone())
        .await
    {
        Ok(r) => r,
        Err(e) => return error_to_problem_json(&e),
    };
    if result.is_new_visit {
        ctx.bus.emit(
            "history.recorded",
            json!({
                "kb": kb_name.as_str(),
                "kind": "open",
                "id": result.id,
                "artifact_id": body.artifact_id,
                "user": user,
            }),
        );
    }
    // RP-track seed-on-open (F6): hand back the visit's accumulated reading
    // state so the iframe runtime resumes its cumulative counters (non-empty
    // only on a 30-min resume). Best-effort — a failure just means the
    // runtime starts fresh and the first beacon re-establishes it.
    let reading = if ctx.reading_progress {
        match ctx.storage.reading_state_for_visit(result.id).await {
            Ok(r) => Some(ReadingResumeState {
                active_ms: r.active_ms,
                last_section: r.last_section,
                sections: r
                    .sections
                    .into_iter()
                    .map(|s| ReadingResumeSection {
                        id: s.section_id,
                        dwell_ms: s.dwell_ms,
                        enters: s.enters,
                    })
                    .collect(),
            }),
            Err(e) => {
                tracing::warn!(error = %e, "reading_state_for_visit failed; runtime starts fresh");
                None
            }
        }
    } else {
        None
    };
    Json(OpenResponse {
        visit_id: result.id,
        scroll_y: result.scroll_y,
        reading,
    })
    .into_response()
}

#[cfg_attr(
    feature = "ts-export",
    derive(ts_rs::TS),
    ts(export, rename = "HistoryScrollBody")
)]
#[derive(Debug, Deserialize)]
pub struct ScrollBody {
    pub visit_id: i64,
    pub scroll_y: i64,
    pub scroll_max: i64,
}

pub async fn post_scroll(
    State(state): State<Arc<KbHandles>>,
    Path(kb): Path<String>,
    Json(body): Json<ScrollBody>,
) -> Response<Body> {
    let (_kb_name, ctx) = match crate::routes::resolve_kb(&state, &kb) {
        Ok(v) => v,
        Err(resp) => return resp,
    };
    if body.scroll_y < 0 || body.scroll_max < 0 {
        return error_to_problem_json(&kb_core::Error::BadRequest(
            "scroll_y and scroll_max must be non-negative".into(),
        ));
    }
    let now_unix = chrono::Utc::now().timestamp();
    match ctx
        .storage
        .history_update_scroll(body.visit_id, body.scroll_y, body.scroll_max, now_unix)
        .await
    {
        Ok(0) => error_to_problem_json(&kb_core::Error::NotFound(format!(
            "visit_id {} not found (or not an open visit)",
            body.visit_id
        ))),
        Ok(_) => StatusCode::NO_CONTENT.into_response(),
        Err(e) => error_to_problem_json(&e),
    }
}

/// Bound on sections per beacon — a healthy doc has tens of headings; this
/// caps an adversarial / runaway client (the rate-limiter is the other
/// backstop).
const MAX_READING_SECTIONS: usize = 5000;

#[cfg_attr(
    feature = "ts-export",
    derive(ts_rs::TS),
    ts(export, rename = "ReadingSectionBeacon")
)]
#[derive(Debug, Deserialize)]
pub struct ReadingSection {
    pub id: String,
    pub idx: i64,
    pub text: String,
    pub level: i64,
    pub words: i64,
    pub content_px: i64,
    pub dwell_ms: i64,
    pub enters: i64,
}

#[cfg_attr(
    feature = "ts-export",
    derive(ts_rs::TS),
    ts(export, rename = "HistoryReadingBody")
)]
#[derive(Debug, Deserialize)]
pub struct ReadingBody {
    pub visit_id: i64,
    pub artifact_id: String,
    #[serde(default)]
    #[cfg_attr(
        feature = "ts-export",
        ts(as = "Option<Vec<ReadingSection>>", optional)
    )]
    pub sections: Vec<ReadingSection>,
    #[serde(default)]
    #[cfg_attr(feature = "ts-export", ts(as = "Option<i64>", optional))]
    pub active_ms: i64,
    #[serde(default)]
    #[cfg_attr(feature = "ts-export", ts(optional))]
    pub last_section: Option<String>,
}

pub async fn post_reading(
    State(state): State<Arc<KbHandles>>,
    Path(kb): Path<String>,
    Json(body): Json<ReadingBody>,
) -> Response<Body> {
    let (_kb_name, ctx) = match crate::routes::resolve_kb(&state, &kb) {
        Ok(v) => v,
        Err(resp) => return resp,
    };
    // Capture disabled for this kb → silently accept + drop so the parent's
    // best-effort POST never logs an error.
    if !ctx.reading_progress {
        return StatusCode::NO_CONTENT.into_response();
    }
    if !is_safe_id(&body.artifact_id) {
        return error_to_problem_json(&kb_core::Error::BadRequest(format!(
            "artifact_id {:?} contains illegal characters",
            body.artifact_id
        )));
    }
    if body.active_ms < 0 {
        return error_to_problem_json(&kb_core::Error::BadRequest(
            "active_ms must be non-negative".into(),
        ));
    }
    if body.sections.len() > MAX_READING_SECTIONS {
        return error_to_problem_json(&kb_core::Error::BadRequest(format!(
            "too many sections ({} > {MAX_READING_SECTIONS})",
            body.sections.len()
        )));
    }
    if body.sections.iter().any(|s| {
        s.dwell_ms < 0
            || s.enters < 0
            || s.words < 0
            || s.content_px < 0
            || s.idx < 0
            || s.level < 0
    }) {
        return error_to_problem_json(&kb_core::Error::BadRequest(
            "reading section fields must be non-negative".into(),
        ));
    }
    let now_unix = chrono::Utc::now().timestamp();
    // set_active FIRST: its rows-affected is the visit-existence 404 gate, and
    // the FK on reading_sections.visit_id would otherwise error on a stale
    // visit before we could 404 cleanly.
    match ctx
        .storage
        .reading_set_active(
            body.visit_id,
            body.active_ms,
            body.last_section.clone(),
            now_unix,
        )
        .await
    {
        Ok(0) => {
            return error_to_problem_json(&kb_core::Error::NotFound(format!(
                "visit_id {} not found (or not an open visit)",
                body.visit_id
            )))
        }
        Ok(_) => {}
        Err(e) => return error_to_problem_json(&e),
    }
    if !body.sections.is_empty() {
        let dwells: Vec<SectionDwell> = body
            .sections
            .iter()
            .map(|s| SectionDwell {
                section_id: s.id.clone(),
                section_idx: s.idx,
                section_text: s.text.clone(),
                level: s.level,
                words: s.words,
                content_px: s.content_px,
                dwell_ms: s.dwell_ms,
                enters: s.enters,
            })
            .collect();
        if let Err(e) = ctx
            .storage
            .reading_upsert_sections(body.visit_id, body.artifact_id.clone(), dwells, now_unix)
            .await
        {
            return error_to_problem_json(&e);
        }
    }
    StatusCode::NO_CONTENT.into_response()
}

#[cfg_attr(
    feature = "ts-export",
    derive(ts_rs::TS),
    ts(export, rename = "HistorySearchBody")
)]
#[derive(Debug, Deserialize)]
pub struct SearchBody {
    pub query: String,
}

#[cfg_attr(
    feature = "ts-export",
    derive(ts_rs::TS),
    ts(export, rename = "HistorySearchResponse")
)]
#[derive(Debug, Serialize)]
pub struct SearchResponse {
    pub id: i64,
}

pub async fn post_search(
    State(state): State<Arc<KbHandles>>,
    Extension(identity): Extension<crate::middleware::Identity>,
    Path(kb): Path<String>,
    Json(body): Json<SearchBody>,
) -> Response<Body> {
    let (kb_name, ctx) = match crate::routes::resolve_kb(&state, &kb) {
        Ok(v) => v,
        Err(resp) => return resp,
    };
    let q = body.query.trim();
    if q.is_empty() {
        return error_to_problem_json(&kb_core::Error::BadRequest(
            "query must be non-empty".into(),
        ));
    }
    if q.len() > 1024 {
        return error_to_problem_json(&kb_core::Error::BadRequest(
            "query exceeds 1024 chars".into(),
        ));
    }
    let now_unix = chrono::Utc::now().timestamp();
    let user = identity.user.clone();
    // history_record_search returns the SAME id for dedup'd entries within
    // the 5s window — we still emit the SSE because the SPA's gallery
    // shows the deduped row's updated_at and benefits from a refresh.
    match ctx
        .storage
        .history_record_search(q.to_string(), now_unix, user.clone())
        .await
    {
        Ok(id) => {
            ctx.bus.emit(
                "history.recorded",
                json!({
                    "kb": kb_name.as_str(),
                    "kind": "search",
                    "id": id,
                    "query": q,
                    "user": user,
                }),
            );
            Json(SearchResponse { id }).into_response()
        }
        Err(e) => error_to_problem_json(&e),
    }
}

#[derive(Debug, Deserialize)]
pub struct ListParams {
    pub limit: Option<u32>,
    pub before: Option<i64>,
    /// `open` | `search` | `comment` | `all` (default).
    pub kind: Option<String>,
    /// v0.34 Y1 — filter to one username. Absent = all users.
    pub user: Option<String>,
}

#[cfg_attr(feature = "ts-export", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Serialize)]
pub struct HistoryEntry {
    pub id: i64,
    #[cfg_attr(
        feature = "ts-export",
        ts(type = "\"open\" | \"search\" | \"comment\"")
    )]
    pub kind: String,
    pub artifact_id: Option<String>,
    pub query: Option<String>,
    pub comment_id: Option<String>,
    pub scroll_y: i64,
    pub scroll_max: i64,
    /// Per-visit high-water mark of `scroll_y`. SPA uses this (not
    /// `scroll_y`) as the numerator for the reading-progress chip so a
    /// fully-read ✓ stays sticky when the user scrolls back up.
    pub scroll_y_max: i64,
    pub started_at: i64,
    pub updated_at: i64,
    /// Enriched title for open/comment rows (None when the artifact
    /// isn't in lance — e.g. it was removed after the visit).
    pub title: Option<String>,
    /// Track U — source-root-relative path for open/comment rows, used
    /// to build the `/a/<kb>/<source_relative>` permalink. Same
    /// nullability as `title` (None when the artifact is gone).
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts-export", ts(optional))]
    pub source_relative: Option<String>,
    /// GC-B5 — who opened this row (`open` kind only): `"web"`, `"cli"`, or
    /// `None` for rows written before the distinction existed / non-open
    /// kinds. Not to be confused with `source_relative` (a path) above.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts-export", ts(optional))]
    pub open_source: Option<String>,
    /// v0.34 Y1/W — attribution username (lowercase). Absent/empty on
    /// pre-multi-user rows; SPA shows a teammate chip when this differs
    /// from the requester's identity.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts-export", ts(optional))]
    pub user: Option<String>,
}

#[cfg_attr(
    feature = "ts-export",
    derive(ts_rs::TS),
    ts(export, rename = "HistoryListResponse")
)]
#[derive(Debug, Serialize)]
pub struct ListResponse {
    pub entries: Vec<HistoryEntry>,
}

pub async fn get_list(
    State(state): State<Arc<KbHandles>>,
    Path(kb): Path<String>,
    Query(params): Query<ListParams>,
) -> Response<Body> {
    let (_kb_name, ctx) = match crate::routes::resolve_kb(&state, &kb) {
        Ok(v) => v,
        Err(resp) => return resp,
    };
    let kind_filter = match params.kind.as_deref() {
        None | Some("all") => None,
        Some(k @ ("open" | "search" | "comment")) => Some(k.to_string()),
        Some(other) => {
            return error_to_problem_json(&kb_core::Error::BadRequest(format!(
                "kind {other:?} must be one of: open, search, comment, all"
            )));
        }
    };
    let limit = params.limit.unwrap_or(DEFAULT_LIMIT).clamp(1, MAX_LIMIT);

    // v0.34 Y1 — `?user=` filters; absent = all users (None).
    let user_filter = params
        .user
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .and_then(kb_core::identity::normalize_username);
    let rows = match ctx
        .storage
        .history_list(limit, params.before, kind_filter, user_filter)
        .await
    {
        Ok(r) => r,
        Err(e) => return error_to_problem_json(&e),
    };

    // Enrich open/comment rows with the artifact title via lance. One bulk
    // `get_by_ids` (`id IN (...)`) over the unique artifact_ids — a single
    // actor round-trip / lance scan instead of one per id (the round-trip
    // count was bounded by `limit` ≤1000). Ids missing from lance (artifact
    // removed after the visit) are simply absent → title/source_relative stay
    // None, same as before.
    let unique_ids: Vec<String> = rows
        .iter()
        .filter_map(|r| r.artifact_id.clone())
        .collect::<std::collections::HashSet<String>>()
        .into_iter()
        .collect();
    let enriched: std::collections::HashMap<String, (String, String)> = ctx
        .storage
        .get_by_ids(unique_ids)
        .await
        .unwrap_or_default()
        .into_iter()
        .map(|doc| {
            let rel = kb_core::paths::doc_rel_path(&doc.path, &ctx.source_path);
            (doc.id, (doc.title, rel))
        })
        .collect();

    let entries = rows
        .into_iter()
        .map(|r| {
            let row = r.artifact_id.as_ref().and_then(|id| enriched.get(id));
            let title = row.map(|(t, _)| t.clone());
            let source_relative = row.map(|(_, rel)| rel.clone());
            HistoryEntry {
                id: r.id,
                kind: r.kind,
                artifact_id: r.artifact_id,
                query: r.query,
                comment_id: r.comment_id,
                scroll_y: r.scroll_y,
                scroll_max: r.scroll_max,
                scroll_y_max: r.scroll_y_max,
                started_at: r.started_at_unix,
                updated_at: r.updated_at_unix,
                title,
                source_relative,
                open_source: r.source,
                // Empty string = pre-multi-user row → omit on the wire.
                user: {
                    let u = r.user.trim();
                    if u.is_empty() {
                        None
                    } else {
                        Some(u.to_string())
                    }
                },
            }
        })
        .collect();
    Json(ListResponse { entries }).into_response()
}

#[derive(Debug, Deserialize)]
pub struct ReadingQuery {
    /// Whole-page numbers only (completion / active time / stop-point); omit
    /// the per-section breakdown + interest ranking. Maps to `kb reading --lite`.
    #[serde(default)]
    pub lite: bool,
}

/// `GET /api/kb/{kb}/artifacts/{id}/reading[?lite=true]` — RP-track reading
/// summary for one artifact, merged across all its visits via
/// `kb_core::reading::summarize`. Returns the summary even when there's no
/// capture yet (zeros / empty sections), so the SPA + CLI degrade cleanly.
pub async fn get_reading(
    State(state): State<Arc<KbHandles>>,
    Extension(identity): Extension<crate::middleware::Identity>,
    Path((kb, id)): Path<(String, String)>,
    Query(q): Query<ReadingQuery>,
) -> Response<Body> {
    let (_kb_name, ctx) = match crate::routes::resolve_kb(&state, &kb) {
        Ok(v) => v,
        Err(resp) => return resp,
    };
    if !is_safe_id(&id) {
        return error_to_problem_json(&kb_core::Error::BadRequest(format!(
            "artifact_id {id:?} contains illegal characters"
        )));
    }
    // v0.34 Y1 — per-user summary for the requester.
    let (sections, visits) = match ctx
        .storage
        .reading_inputs_for_artifact(id, Some(identity.user.clone()))
        .await
    {
        Ok(v) => v,
        Err(e) => return error_to_problem_json(&e),
    };
    let summary = kb_core::reading::summarize(&sections, &visits, q.lite);
    Json(summary).into_response()
}

/// W2.10 — bounds the `/history/calendar` window so the `GROUP BY` scan
/// (and the SPA's year-grid) stay bounded. A shade over a year of slack past
/// 365 days, not a hard "exactly one year" — the density grid renders
/// whatever comes back, it doesn't assume a fixed cell count.
const MAX_CALENDAR_SPAN_SECS: i64 = 400 * 24 * 60 * 60;
/// Default lookback when `from` is omitted: the last 365 days up to `to`.
const DEFAULT_CALENDAR_SPAN_SECS: i64 = 365 * 24 * 60 * 60;

#[derive(Debug, Deserialize)]
pub struct CalendarParams {
    /// Unix seconds, inclusive. Defaults to `to - 365d`.
    pub from: Option<i64>,
    /// Unix seconds, inclusive. Defaults to now.
    pub to: Option<i64>,
}

/// One UTC calendar day's event counts (W2.10). `day` is `YYYY-MM-DD` in
/// UTC — see `kb_core::storage::sqlite::Db::history_counts_by_day`'s doc
/// comment for why (matches the `echoes` route's UTC convention; the SPA
/// labels the grid as UTC rather than reinterpreting these locally).
#[cfg_attr(feature = "ts-export", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Clone, Serialize)]
pub struct CalendarDay {
    pub day: String,
    pub opens: i64,
    pub searches: i64,
    pub comments: i64,
}

#[cfg_attr(
    feature = "ts-export",
    derive(ts_rs::TS),
    ts(export, rename = "HistoryCalendarResponse")
)]
#[derive(Debug, Clone, Serialize)]
pub struct CalendarResponse {
    pub days: Vec<CalendarDay>,
}

/// `GET /api/kb/{kb}/history/calendar?from=&to=` — per-UTC-day density for
/// the gallery's activity calendar (W2.10). One bounded `GROUP BY` over the
/// `history` table (`Db::history_counts_by_day`); pivots the per-`(day,
/// kind)` rows into one `CalendarDay` per day. `from`/`to` default to a
/// trailing 365-day window ending now; the requested span is capped at
/// [`MAX_CALENDAR_SPAN_SECS`] to keep the scan (and the SPA's grid) bounded.
/// Read-only — tolerates an empty/purged history table (an empty `days`).
pub async fn get_calendar(
    State(state): State<Arc<KbHandles>>,
    Path(kb): Path<String>,
    Query(params): Query<CalendarParams>,
) -> Response<Body> {
    let (_kb_name, ctx) = match crate::routes::resolve_kb(&state, &kb) {
        Ok(v) => v,
        Err(resp) => return resp,
    };
    let now = chrono::Utc::now().timestamp();
    let to = params.to.unwrap_or(now);
    let from = params.from.unwrap_or(to - DEFAULT_CALENDAR_SPAN_SECS);
    if to < from {
        return error_to_problem_json(&kb_core::Error::BadRequest("to must be >= from".into()));
    }
    if to - from > MAX_CALENDAR_SPAN_SECS {
        return error_to_problem_json(&kb_core::Error::BadRequest(format!(
            "requested range exceeds the {MAX_CALENDAR_SPAN_SECS}s cap"
        )));
    }

    let rows = match ctx.storage.history_counts_by_day(from, to).await {
        Ok(r) => r,
        Err(e) => return error_to_problem_json(&e),
    };
    Json(CalendarResponse {
        days: pivot_calendar_days(rows),
    })
    .into_response()
}

/// Pivot `Db::history_counts_by_day`'s per-`(day, kind)` rows into one
/// [`CalendarDay`] per day. Pure — unit-tested directly (the route wraps it
/// with the actor round-trip). Days come out sorted ascending regardless of
/// input order (a `BTreeMap` merge point); an unrecognised `kind` (schema
/// drift) is silently dropped rather than panicking a read route.
fn pivot_calendar_days(rows: Vec<kb_core::storage::sqlite::DayKindCount>) -> Vec<CalendarDay> {
    let mut by_day: std::collections::BTreeMap<String, CalendarDay> =
        std::collections::BTreeMap::new();
    for r in rows {
        let entry = by_day.entry(r.day.clone()).or_insert_with(|| CalendarDay {
            day: r.day.clone(),
            opens: 0,
            searches: 0,
            comments: 0,
        });
        match r.kind.as_str() {
            "open" => entry.opens = r.count,
            "search" => entry.searches = r.count,
            "comment" => entry.comments = r.count,
            _ => {}
        }
    }
    by_day.into_values().collect()
}

/// `POST /api/kb/{kb}/history/purge` — S5 admin: drop every row from
/// the history table for this kb. The reverse of `history_record_*`;
/// no granular range/kind filter — full wipe only. Emits the
/// `history.purged` SSE so subscribers refresh.
pub async fn purge(State(state): State<Arc<KbHandles>>, Path(kb): Path<String>) -> Response<Body> {
    let (kb_name, ctx) = match crate::routes::resolve_kb(&state, &kb) {
        Ok(v) => v,
        Err(resp) => return resp,
    };
    let n = match ctx.storage.history_purge().await {
        Ok(n) => n,
        Err(e) => return error_to_problem_json(&e),
    };
    ctx.bus.emit(
        "history.purged",
        json!({"kb": kb_name.as_str(), "rows_deleted": n}),
    );
    (
        StatusCode::OK,
        Json(json!({"kb": kb_name.as_str(), "rows_deleted": n})),
    )
        .into_response()
}

#[cfg(test)]
mod tests {
    use super::*;
    use kb_core::storage::sqlite::DayKindCount;

    fn dkc(day: &str, kind: &str, count: i64) -> DayKindCount {
        DayKindCount {
            day: day.into(),
            kind: kind.into(),
            count,
        }
    }

    #[test]
    fn pivot_calendar_days_merges_kinds_per_day() {
        let days = pivot_calendar_days(vec![
            dkc("2021-01-01", "open", 2),
            dkc("2021-01-01", "search", 1),
            dkc("2021-01-02", "comment", 3),
        ]);
        assert_eq!(days.len(), 2);
        assert_eq!(days[0].day, "2021-01-01");
        assert_eq!(days[0].opens, 2);
        assert_eq!(days[0].searches, 1);
        assert_eq!(days[0].comments, 0);
        assert_eq!(days[1].day, "2021-01-02");
        assert_eq!(days[1].opens, 0);
        assert_eq!(days[1].comments, 3);
    }

    #[test]
    fn pivot_calendar_days_sorts_ascending_regardless_of_input_order() {
        let days = pivot_calendar_days(vec![
            dkc("2021-06-01", "open", 1),
            dkc("2021-01-01", "open", 1),
            dkc("2021-03-01", "open", 1),
        ]);
        let ordered: Vec<&str> = days.iter().map(|d| d.day.as_str()).collect();
        assert_eq!(ordered, ["2021-01-01", "2021-03-01", "2021-06-01"]);
    }

    #[test]
    fn pivot_calendar_days_empty_input_is_empty() {
        assert!(pivot_calendar_days(vec![]).is_empty());
    }

    #[test]
    fn pivot_calendar_days_drops_unrecognised_kind() {
        // A future schema drift (a new history `kind`) must not panic a
        // read route — the day still shows up with zero counts.
        let days = pivot_calendar_days(vec![dkc("2021-01-01", "bogus-future-kind", 9)]);
        assert_eq!(days.len(), 1);
        assert_eq!(days[0].opens, 0);
        assert_eq!(days[0].searches, 0);
        assert_eq!(days[0].comments, 0);
    }
}
