//! `GET /api/kb/{kb}/daycard` — the e-ink daycard: a deterministic,
//! self-contained HTML+inline-SVG "day at a glance" document, or its JSON
//! data twin. Recorded design constraint (Wave 4 synthesis, kept verbatim):
//! *the daycard serves deterministic HTML/SVG that the panel rasterizes; no
//! daemon bitmap pipeline* — this route emits MARKUP, never a PNG/BMP, and
//! pulls in zero new media dependencies. A physical e-ink panel (or a kiosk
//! browser on a spare monitor) points its ONE fixed URL at this route with
//! no query params and gets the HTML branch (see [`wants_json`]); the SPA's
//! `/ambient` route and `kb daycard` both ask for the JSON branch
//! explicitly.
//!
//! A VIEW over EXISTING primitives only — no new aggregation, no new
//! storage, no daemon-side scoring beyond what already ships:
//!  - "worth picking back up" = [`crate::routes::resurface::compute_items`],
//!    the SAME deterministic scorer the `/resurface` route and `kb
//!    resurface` use (top [`DAYCARD_RESURFACE_LIMIT`] items only — an e-ink
//!    panel has no scroll).
//!  - "today" activity = one `history_counts_by_day` scan (W2.10, the same
//!    call `history::get_calendar` and `timeline::list` use), windowed to
//!    exactly the requested UTC day.
//!  - "recent" / "never opened" = the shared gallery row-set memo (PF-R1;
//!    `docs::gallery_snapshot`, invariant #15 — the facets/resurface
//!    pattern's `list_docs` scan, but shared with `/docs`/`/facets`/`/edges`
//!    instead of re-pulled) cross-referenced against one `reading_rollup`
//!    scan — the same two signals `resurface::compute_items` already reads,
//!    read again here for the (different) doc-picking purpose. Accepted as
//!    a second, low-QPS read (an ambient/e-ink poller, not a hot path)
//!    rather than reshaping `compute_items`'s return type.
//!
//! **Deterministic given `(corpus state, day)`**: the caller-supplied (or
//! UTC-today-defaulted) calendar day is the ONLY clock input — every
//! `now_unix` fed to `resurface::compute_items` below is that day's UTC
//! midnight, never wall-clock time, so two requests against an unchanged
//! corpus for the same day produce byte-identical JSON and byte-identical
//! HTML. `render_html_is_deterministic_for_same_input` pins the rendering
//! half of that contract directly; the storage-actor half is exactly
//! `resurface::compute_items`'s own (already-tested) contract.
//!
//! **Pull-only, anti-nag**: no push, no "you haven't opened this in N days"
//! language anywhere in the copy — the `resurface`/`echoes` non-goals apply
//! here verbatim (see their module docs). **NO streaks, NO goals, NO
//! completion percentages, NO badges** — the activity section is a plain
//! count of what happened today, nothing more; there is deliberately no
//! "day N" or celebratory framing anywhere in [`render_html`].
//!
//! E-ink friendly: system-font stack only (no `@font-face`/web fonts), pure
//! black-on-white (no colour-only signal), no `<script>`, everything inlined
//! (one URL, one response, no follow-on asset fetches, no JS needed to read
//! it).
//!
//! Per-kb only, matching the `resurface`/`echoes`/`timeline` family it reuses
//! — a federated "every kb's daycard" digest would need `routes::
//! buffered_join` (invariant #28) and is out of scope for this thin slice.
//!
//! **CT-E1 — `?since=`, "what happened while I was away"**: additive,
//! mutually exclusive with `?day=` (400 on both). Composes FOUR lanes from
//! existing reads only, no new storage: sessions captured in-window
//! (`sessions_list` + the #11 newest-capture collapse — a local twin of
//! `echoes`/`timeline`'s own copy of that small pure helper; this route
//! family duplicates it per-module rather than exposing it cross-module,
//! matching e.g. `echoes::cwd_basename`'s precedent); memory-note docs by
//! CREATED time (`created_unix`, falling back to `mtime_unix` — the same
//! fallback `routes::memory` uses); every OTHER doc by `mtime_unix`
//! (created-or-updated, invariant #35's axis); and comments RAISED in-window
//! (`history_comments_in_window` — kb-comments/1 has no `resolved_at`, so
//! "raised" is the only honest window signal, matching `timeline`'s
//! `comment` lane) cross-referenced against the SAME `.review/` walk
//! `/inbox`/`/resurface` already use ([`crate::routes::inbox::collect_open`])
//! to answer "still open" — never a second bespoke walk. Unlike day mode,
//! `to_unix` is wall-clock "now" at request time, NOT reproducible across
//! requests — the whole point is "since then, up to right now".

use crate::middleware::error_to_problem_json;
use crate::routes::inbox::{collect_open, InboxItem};
use crate::routes::resurface::ResurfaceItem;
use crate::state::KbHandles;
use axum::{
    body::Body,
    extract::{Extension, Path, Query, State},
    http::{header, HeaderMap, Response},
    response::IntoResponse,
    Json,
};
use kb_core::storage::lance::DocSummary;
use kb_core::storage::sqlite::{DayKindCount, HistoryRow, SessionRow};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::sync::Arc;

/// The panel has no scroll — keep the "worth picking back up" section to a
/// glance, not a list. Sibling of `resurface::DEFAULT_LIMIT` (8) /
/// `echoes::DEFAULT_LIMIT` (4), deliberately smaller than either.
const DAYCARD_RESURFACE_LIMIT: usize = 3;
/// Cap on each of the "recent" / "never opened" sections — same "a glance,
/// not a list" reasoning as above.
const DAYCARD_DOC_LIMIT: usize = 3;
/// CT-E1 — cap on each `?since=` lane (sessions / memories / artifacts /
/// raised comments). Far more generous than [`DAYCARD_DOC_LIMIT`]: a
/// since-window answers "what happened while I was away", which can
/// legitimately span days, so this is headroom rather than a meaningful cap
/// in the common case — same "ambient poller, not a hot path" cost posture
/// as the rest of this route.
const DAYCARD_SINCE_LANE_CAP: usize = 100;

#[cfg_attr(feature = "ts-export", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Serialize)]
pub struct DaycardDoc {
    pub id: String,
    pub title: String,
    /// Source-relative path for the SPA `/a/{kb}/{rel}` deep-link.
    pub source_relative: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts-export", ts(optional))]
    pub mtime_unix: Option<i64>,
}

#[cfg_attr(feature = "ts-export", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Serialize, Clone, Copy, Default)]
pub struct DaycardActivity {
    pub opens: i64,
    pub searches: i64,
    pub comments: i64,
}

#[cfg_attr(
    feature = "ts-export",
    derive(ts_rs::TS),
    ts(export, rename = "DaycardResponse")
)]
#[derive(Debug, Serialize)]
pub struct DaycardResponse {
    pub kb: String,
    /// UTC calendar day this card was built for, `YYYY-MM-DD`.
    pub day: String,
    /// The clock every score/age on this card was computed against — that
    /// day's UTC midnight, NOT wall-clock time (the determinism contract).
    pub now_unix: i64,
    /// Top [`DAYCARD_RESURFACE_LIMIT`] items from the SAME scorer
    /// `/resurface` uses — see the module doc.
    pub resurface: Vec<ResurfaceItem>,
    pub activity: DaycardActivity,
    /// Most recently modified artifacts NOT already in `resurface`,
    /// deterministically ordered (`mtime_unix` desc, id asc tiebreak).
    pub recent: Vec<DaycardDoc>,
    /// Artifacts with zero recorded opens, NOT already in `resurface` or
    /// `recent`, same deterministic ordering.
    pub never_opened: Vec<DaycardDoc>,
}

#[derive(Debug, Deserialize, Default)]
pub struct DaycardParams {
    /// `YYYY-MM-DD`, UTC. Defaults to today (UTC). Mutually exclusive with
    /// `since` (400 when both are present).
    pub day: Option<String>,
    /// CT-E1 — unix seconds or a bare `YYYY-MM-DD` UTC calendar date (UTC
    /// midnight): the window lower bound for "what happened while I was
    /// away". Mutually exclusive with `day` (400 when both are present).
    pub since: Option<String>,
    /// Explicit content-negotiation override: `"json"` or `"html"`.
    /// Absent → decided from the `Accept` header (see [`wants_json`]).
    pub format: Option<String>,
}

/// CT-E1 — one session captured in the `?since=` window.
#[cfg_attr(feature = "ts-export", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Serialize)]
pub struct DaycardSessionItem {
    /// Deep-link handle: the SPA resolves `/sessions?focus=<session_id>`.
    pub session_id: String,
    pub title: String,
    /// Raw wire value — `None` means un-backfilled history; every reader
    /// must treat that as `"substantive"` (matches `SessionRow::substance`'s
    /// own contract). Not pre-normalized here, same as `routes::sessions::list`.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts-export", ts(optional))]
    pub substance: Option<String>,
    /// Capture time (unix secs) — the axis this lane filters on.
    pub started_at: i64,
}

/// CT-E1 — one comment thread raised in the `?since=` window.
#[cfg_attr(feature = "ts-export", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Serialize)]
pub struct DaycardCommentItem {
    pub comment_id: String,
    pub artifact_id: String,
    pub title: String,
    /// `None` when the artifact has left lance (the review file outlived
    /// it) — same non-navigable-row semantics as `routes::inbox::InboxItem`.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts-export", ts(optional))]
    pub source_relative: Option<String>,
    pub raised_at: i64,
    /// Still open AS OF THIS REQUEST (cross-referenced against
    /// `routes::inbox::collect_open`'s live `.review/` walk) — kb-comments/1
    /// has no `resolved_at`, so this is the only honest "still open" signal.
    pub open: bool,
}

#[cfg_attr(
    feature = "ts-export",
    derive(ts_rs::TS),
    ts(export, rename = "DaycardSinceResponse")
)]
#[derive(Debug, Serialize)]
pub struct DaycardSinceResponse {
    pub kb: String,
    /// Window lower bound, verbatim from `?since=`.
    pub since_unix: i64,
    /// Window upper bound — wall-clock "now" at request time. Unlike the day
    /// mode's clocked-to-midnight determinism contract, this is deliberately
    /// NOT reproducible across requests (see the module doc).
    pub to_unix: i64,
    pub sessions: Vec<DaycardSessionItem>,
    /// `true` when more than [`DAYCARD_SINCE_LANE_CAP`] sessions matched —
    /// the list above is the newest [`DAYCARD_SINCE_LANE_CAP`], never a
    /// silent drop.
    pub sessions_truncated: bool,
    /// Memory-note docs (memory-category EXCEPT `memory-session`, which has
    /// its own `sessions` lane above) whose created time falls in the
    /// window.
    pub memories: Vec<DaycardDoc>,
    pub memories_truncated: bool,
    /// Every other (non-memory, non-session) doc whose `mtime_unix` falls in
    /// the window.
    pub artifacts: Vec<DaycardDoc>,
    pub artifacts_truncated: bool,
    pub comments: Vec<DaycardCommentItem>,
    pub comments_truncated: bool,
    /// Count of `comments` still open as of this request — scoped to what
    /// was RAISED in this window only (not a corpus-wide open count; that's
    /// `/resurface`'s/`/inbox`'s job).
    pub comments_still_open: u32,
}

pub async fn get(
    State(state): State<Arc<KbHandles>>,
    Extension(identity): Extension<crate::middleware::Identity>,
    Path(kb): Path<String>,
    Query(params): Query<DaycardParams>,
    headers: HeaderMap,
) -> Response<Body> {
    let (kb_name, ctx) = match crate::routes::resolve_kb(&state, &kb) {
        Ok(v) => v,
        Err(resp) => return resp,
    };
    let user = identity.user.clone();

    // CT-E1 — day/since are mutually exclusive views (one whole day vs. a
    // rolling window); a caller sending both gets a 400, never a silent
    // pick-one.
    if let Err(e) = validate_day_since_exclusive(&params.day, &params.since) {
        return error_to_problem_json(&e);
    }

    if let Some(raw) = params.since.as_deref() {
        let since_unix = match parse_since_bound(raw) {
            Ok(v) => v,
            Err(e) => return error_to_problem_json(&e),
        };
        let to_unix = chrono::Utc::now().timestamp();
        if since_unix > to_unix {
            return error_to_problem_json(&kb_core::Error::BadRequest(
                "since must not be in the future".to_string(),
            ));
        }
        let response = match build_since_response(&state, &kb_name, ctx, since_unix, to_unix).await
        {
            Ok(v) => v,
            Err(e) => return error_to_problem_json(&e),
        };
        return if wants_json(&headers, &params) {
            Json(response).into_response()
        } else {
            let html = render_since_html(&response);
            let mut resp = Response::new(Body::from(html));
            resp.headers_mut().insert(
                header::CONTENT_TYPE,
                header::HeaderValue::from_static("text/html; charset=utf-8"),
            );
            resp
        };
    }

    let day = match params.day.as_deref().map(parse_day).transpose() {
        Ok(v) => v.unwrap_or_else(|| chrono::Utc::now().date_naive()),
        Err(e) => return error_to_problem_json(&e),
    };
    let day_start = match day.and_hms_opt(0, 0, 0) {
        Some(dt) => dt.and_utc().timestamp(),
        None => {
            return error_to_problem_json(&kb_core::Error::BadRequest("invalid day".to_string()))
        }
    };
    let day_end = day_start + 86_399;

    // "Worth picking back up" — the exact resurface scorer, clocked to this
    // day's UTC midnight rather than wall time (determinism).
    let resurface = match crate::routes::resurface::compute_items(
        &state,
        &kb_name,
        ctx,
        DAYCARD_RESURFACE_LIMIT,
        day_start,
        user.clone(),
    )
    .await
    {
        Ok(v) => v,
        Err(e) => return error_to_problem_json(&e),
    };
    let already: HashSet<&str> = resurface.iter().map(|r| r.id.as_str()).collect();

    // Today's activity — one bounded GROUP BY, same call the calendar +
    // timeline routes use, windowed to exactly this one UTC day.
    let day_kind_rows = match ctx.storage.history_counts_by_day(day_start, day_end).await {
        Ok(r) => r,
        Err(e) => return error_to_problem_json(&e),
    };
    let activity = pivot_activity(&day_kind_rows);

    // Recent / never-opened picks — the shared gallery row-set memo (PF-R1;
    // invariant #15) + one reading_rollup scan (the facets/resurface
    // pattern), cross-referenced. Reusing the memo instead of an
    // independent `list_docs(u32::MAX)` actor round-trip means this
    // low-QPS, ambient-poller route (see module doc) rides whatever the
    // higher-traffic `/docs`/`/facets`/`/edges` routes keep warm; the
    // helper below borrows a plain `&[DocSummary]`, so the (bounded, cheap
    // relative to the actor round-trip it replaces) rows are cloned out of
    // the `Arc`'d memo once here.
    let (gallery_rows, _edge_counts) = match crate::routes::docs::gallery_snapshot(ctx).await {
        Ok(v) => v,
        Err(e) => return error_to_problem_json(&e),
    };
    let docs: Vec<DocSummary> = gallery_rows.iter().map(|r| r.doc.clone()).collect();
    let rollup = match ctx.storage.reading_rollup(user).await {
        Ok(r) => r,
        Err(e) => return error_to_problem_json(&e),
    };
    let (recent, never_opened) =
        pick_recent_and_never_opened(&docs, &rollup, &already, &ctx.source_path);

    let response = DaycardResponse {
        kb: kb_name.as_str().to_string(),
        day: day.format("%Y-%m-%d").to_string(),
        now_unix: day_start,
        resurface,
        activity,
        recent,
        never_opened,
    };

    if wants_json(&headers, &params) {
        Json(response).into_response()
    } else {
        let html = render_html(&response);
        let mut resp = Response::new(Body::from(html));
        resp.headers_mut().insert(
            header::CONTENT_TYPE,
            header::HeaderValue::from_static("text/html; charset=utf-8"),
        );
        resp
    }
}

/// Parse a bare `YYYY-MM-DD` UTC calendar date — the daycard's `?day=` is
/// always a whole day (never a unix-seconds instant, unlike `timeline`'s
/// `from`/`to`), since the whole route's contract is "one document per
/// day", not an arbitrary instant.
fn parse_day(s: &str) -> Result<chrono::NaiveDate, kb_core::Error> {
    chrono::NaiveDate::parse_from_str(s, "%Y-%m-%d").map_err(|e| {
        kb_core::Error::BadRequest(format!("invalid day {s:?}: expected YYYY-MM-DD ({e})"))
    })
}

/// CT-E1 — `day` and `since` are two mutually exclusive views (one whole
/// UTC day vs. a rolling window); reject BOTH present rather than silently
/// preferring one. Pure/testable without a DB.
fn validate_day_since_exclusive(
    day: &Option<String>,
    since: &Option<String>,
) -> Result<(), kb_core::Error> {
    if day.is_some() && since.is_some() {
        return Err(kb_core::Error::BadRequest(
            "day and since are mutually exclusive".to_string(),
        ));
    }
    Ok(())
}

/// CT-E1 — parse `?since=`: unix seconds or a bare `YYYY-MM-DD` UTC calendar
/// date (UTC midnight). A local twin of `timeline::parse_bound` (same
/// grammar, duplicated rather than exposed cross-module per this route
/// family's locality convention, e.g. `echoes::cwd_basename`).
fn parse_since_bound(s: &str) -> Result<i64, kb_core::Error> {
    if let Ok(secs) = s.parse::<i64>() {
        return Ok(secs);
    }
    let date = chrono::NaiveDate::parse_from_str(s, "%Y-%m-%d").map_err(|e| {
        kb_core::Error::BadRequest(format!(
            "invalid since {s:?}: expected unix seconds or YYYY-MM-DD ({e})"
        ))
    })?;
    let dt = date
        .and_hms_opt(0, 0, 0)
        .ok_or_else(|| kb_core::Error::BadRequest(format!("invalid date {s:?}")))?;
    Ok(dt.and_utc().timestamp())
}

/// Content-negotiation: `?format=` always wins when present; otherwise an
/// `Accept` header naming `application/json` (the SPA's `get<T>` fetch
/// wrapper always sends exactly that) selects JSON. Everything else —
/// including no `Accept` header at all, what a bare e-ink HTTP client sends
/// — defaults to HTML, so the panel's one configured URL needs zero query
/// params.
fn wants_json(headers: &HeaderMap, params: &DaycardParams) -> bool {
    match params.format.as_deref() {
        Some("json") => return true,
        Some("html") => return false,
        _ => {}
    }
    headers
        .get(header::ACCEPT)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|a| a.to_ascii_lowercase().contains("application/json"))
}

/// Pivot `Db::history_counts_by_day`'s per-`(day, kind)` rows (windowed to
/// exactly one UTC day by the caller) into the three activity counts. Pure —
/// mirrors `history::pivot_calendar_days` / `timeline::pivot_day_kind_counts`
/// but collapses straight to totals since the daycard shows one day, not a
/// grid.
fn pivot_activity(rows: &[DayKindCount]) -> DaycardActivity {
    let mut activity = DaycardActivity::default();
    for r in rows {
        match r.kind.as_str() {
            "open" => activity.opens += r.count,
            "search" => activity.searches += r.count,
            "comment" => activity.comments += r.count,
            _ => {}
        }
    }
    activity
}

/// Pick the "recent" and "never opened" sections: memory-category docs are
/// excluded (their own attention economy, matching `resurface`'s exclusion),
/// as is anything already surfaced in `exclude` (the resurface picks — no
/// point repeating a title across two sections on a panel with no scroll).
/// Both lists share ONE deterministic order (`mtime_unix` desc, id asc
/// tiebreak) built once; `recent` takes the head, `never_opened` walks the
/// same order looking for docs absent from `rollup` (never actually opened)
/// that `recent` didn't already claim.
fn pick_recent_and_never_opened(
    docs: &[DocSummary],
    rollup: &std::collections::HashMap<String, kb_core::reading::ReadRollup>,
    exclude: &HashSet<&str>,
    source_path: &std::path::Path,
) -> (Vec<DaycardDoc>, Vec<DaycardDoc>) {
    let mut candidates: Vec<&DocSummary> = docs
        .iter()
        .filter(|d| {
            !d.kb_category
                .as_deref()
                .is_some_and(|c| c.starts_with("memory"))
                && !exclude.contains(d.id.as_str())
        })
        .collect();
    candidates.sort_by(|a, b| {
        b.mtime_unix
            .cmp(&a.mtime_unix)
            .then_with(|| a.id.cmp(&b.id))
    });

    let mut recent = Vec::new();
    let mut claimed: HashSet<&str> = HashSet::new();
    for d in &candidates {
        if recent.len() >= DAYCARD_DOC_LIMIT {
            break;
        }
        recent.push(to_daycard_doc(d, source_path));
        claimed.insert(d.id.as_str());
    }

    let mut never_opened = Vec::new();
    for d in &candidates {
        if never_opened.len() >= DAYCARD_DOC_LIMIT {
            break;
        }
        if claimed.contains(d.id.as_str()) || rollup.contains_key(&d.id) {
            continue;
        }
        never_opened.push(to_daycard_doc(d, source_path));
    }

    (recent, never_opened)
}

fn to_daycard_doc(d: &DocSummary, source_path: &std::path::Path) -> DaycardDoc {
    DaycardDoc {
        id: d.id.clone(),
        title: d.title.clone(),
        source_relative: kb_core::paths::doc_rel_path(&d.path, source_path),
        mtime_unix: d.mtime_unix,
    }
}

/// CT-E1 — assemble the `?since=` response: four independent reads (sessions
/// full scan, the shared gallery row-set memo feeding both the memories and
/// artifacts lanes, `history_comments_in_window`, and the `collect_open`
/// `.review/` walk), each folded through a pure/testable helper below.
async fn build_since_response(
    state: &KbHandles,
    kb_name: &kb_core::types::KbName,
    ctx: &crate::state::KbContext,
    since_unix: i64,
    to_unix: i64,
) -> Result<DaycardSinceResponse, kb_core::Error> {
    let sessions_raw = ctx
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
        .await?;
    let (sessions, sessions_truncated) =
        filter_sessions_in_window(sessions_raw, since_unix, to_unix);

    // The shared gallery row-set memo (PF-R1; invariant #15) feeds BOTH the
    // memories and artifacts lanes (the facets/resurface pattern this route
    // already follows for day mode) — reused instead of an independent
    // `list_docs(u32::MAX)` actor round-trip, same memo the day-mode branch
    // above and `/docs`/`/facets`/`/edges` keep warm.
    let (gallery_rows, _edge_counts) = crate::routes::docs::gallery_snapshot(ctx).await?;
    let docs: Vec<DocSummary> = gallery_rows.iter().map(|r| r.doc.clone()).collect();
    let (memories, memories_truncated, artifacts, artifacts_truncated) =
        filter_docs_in_window(&docs, since_unix, to_unix, &ctx.source_path);

    let comment_rows = ctx
        .storage
        .history_comments_in_window(since_unix, to_unix, DAYCARD_SINCE_LANE_CAP as u32)
        .await?;
    let comments_truncated = comment_rows.len() == DAYCARD_SINCE_LANE_CAP;
    let review_dir = state.paths.kb_review_dir(kb_name);
    let open_items = collect_open(kb_name.as_str(), ctx, &review_dir).await;
    let docs_by_id: HashMap<&str, &DocSummary> = docs.iter().map(|d| (d.id.as_str(), d)).collect();
    let (comments, comments_still_open) =
        build_comment_items(&comment_rows, &open_items, &docs_by_id, &ctx.source_path);

    Ok(DaycardSinceResponse {
        kb: kb_name.as_str().to_string(),
        since_unix,
        to_unix,
        sessions,
        sessions_truncated,
        memories,
        memories_truncated,
        artifacts,
        artifacts_truncated,
        comments,
        comments_truncated,
        comments_still_open,
    })
}

/// `sessions` lane — collapse multi-capture (#11) then keep only sessions
/// whose `started_at` falls in `[since_unix, to_unix]`, deterministically
/// ordered (`started_at` desc, `session_id` asc tiebreak) and capped at
/// [`DAYCARD_SINCE_LANE_CAP`]. Pure/testable without a DB, matching
/// `echoes`/`timeline`'s own window-filter helpers.
fn filter_sessions_in_window(
    rows: Vec<SessionRow>,
    since_unix: i64,
    to_unix: i64,
) -> (Vec<DaycardSessionItem>, bool) {
    let mut items: Vec<DaycardSessionItem> = collapse_newest_capture(rows)
        .into_iter()
        .filter(|r| r.started_at >= since_unix && r.started_at <= to_unix)
        .map(|r| DaycardSessionItem {
            session_id: r.session_id.clone(),
            title: session_title(&r),
            substance: r.substance.clone(),
            started_at: r.started_at,
        })
        .collect();
    items.sort_by(|a, b| {
        b.started_at
            .cmp(&a.started_at)
            .then_with(|| a.session_id.cmp(&b.session_id))
    });
    let truncated = items.len() > DAYCARD_SINCE_LANE_CAP;
    items.truncate(DAYCARD_SINCE_LANE_CAP);
    (items, truncated)
}

/// `memories` + `artifacts` lanes from ONE `list_docs` scan: memory-NOTE
/// docs (excluding `memory-session`, which has its own `sessions` lane)
/// bucket into `memories` keyed on CREATED time (`created_unix`, falling
/// back to `mtime_unix` for pre-v0.15 rows — same fallback `routes::memory`
/// uses); every other non-memory doc buckets into `artifacts` keyed on
/// `mtime_unix` (created OR updated, invariant #35's axis). Both share ONE
/// deterministic order (`mtime_unix` desc, id asc tiebreak) and
/// [`DAYCARD_SINCE_LANE_CAP`]. Pure/testable without a DB.
fn filter_docs_in_window(
    docs: &[DocSummary],
    since_unix: i64,
    to_unix: i64,
    source_path: &std::path::Path,
) -> (Vec<DaycardDoc>, bool, Vec<DaycardDoc>, bool) {
    let mut memories: Vec<DaycardDoc> = Vec::new();
    let mut artifacts: Vec<DaycardDoc> = Vec::new();
    for d in docs {
        let category = d.kb_category.as_deref();
        if is_memory_note_category(category) {
            let Some(created) = d.created_unix.or(d.mtime_unix) else {
                continue;
            };
            if created >= since_unix && created <= to_unix {
                memories.push(DaycardDoc {
                    id: d.id.clone(),
                    title: d.title.clone(),
                    source_relative: kb_core::paths::doc_rel_path(&d.path, source_path),
                    mtime_unix: Some(created),
                });
            }
        } else if !category.is_some_and(|c| c.starts_with("memory")) {
            if let Some(mtime) = d.mtime_unix {
                if mtime >= since_unix && mtime <= to_unix {
                    artifacts.push(to_daycard_doc(d, source_path));
                }
            }
        }
    }
    let order = |a: &DaycardDoc, b: &DaycardDoc| {
        b.mtime_unix
            .cmp(&a.mtime_unix)
            .then_with(|| a.id.cmp(&b.id))
    };
    memories.sort_by(order);
    artifacts.sort_by(order);
    let memories_truncated = memories.len() > DAYCARD_SINCE_LANE_CAP;
    memories.truncate(DAYCARD_SINCE_LANE_CAP);
    let artifacts_truncated = artifacts.len() > DAYCARD_SINCE_LANE_CAP;
    artifacts.truncate(DAYCARD_SINCE_LANE_CAP);
    (memories, memories_truncated, artifacts, artifacts_truncated)
}

/// `comments` lane: cross-reference comments RAISED in-window
/// (`history_comments_in_window` rows — kb-comments/1 has no `resolved_at`,
/// so "raised" is the only honest window signal) against the SAME
/// `.review/` walk `/inbox`/`/resurface` already use ([`collect_open`]) to
/// answer "still open" — never a second bespoke walk. Pure/testable without
/// a DB (the caller does the two async reads; this just joins them).
fn build_comment_items(
    rows: &[HistoryRow],
    open_items: &[InboxItem],
    docs_by_id: &HashMap<&str, &DocSummary>,
    source_path: &std::path::Path,
) -> (Vec<DaycardCommentItem>, u32) {
    let open_by_comment: HashMap<&str, &InboxItem> = open_items
        .iter()
        .map(|it| (it.comment_id.as_str(), it))
        .collect();
    let mut items: Vec<DaycardCommentItem> = Vec::new();
    for row in rows {
        let (Some(comment_id), Some(artifact_id)) =
            (row.comment_id.clone(), row.artifact_id.clone())
        else {
            continue;
        };
        let (title, source_relative, open) =
            if let Some(item) = open_by_comment.get(comment_id.as_str()) {
                (item.title.clone(), item.source_relative.clone(), true)
            } else if let Some(doc) = docs_by_id.get(artifact_id.as_str()) {
                (
                    doc.title.clone(),
                    Some(kb_core::paths::doc_rel_path(&doc.path, source_path)),
                    false,
                )
            } else {
                (artifact_id.clone(), None, false)
            };
        items.push(DaycardCommentItem {
            comment_id,
            artifact_id,
            title,
            source_relative,
            raised_at: row.started_at_unix,
            open,
        });
    }
    items.sort_by(|a, b| {
        b.raised_at
            .cmp(&a.raised_at)
            .then_with(|| a.comment_id.cmp(&b.comment_id))
    });
    let still_open = items.iter().filter(|c| c.open).count() as u32;
    (items, still_open)
}

/// Narrow "this IS a memory-note artifact" gate — `memory-*` EXCEPT
/// `memory-session` (session transcripts have their own `sessions` lane). A
/// local twin of `echoes::is_memory_category` (small pure helper, duplicated
/// rather than exposed cross-module per this route family's locality
/// convention).
fn is_memory_note_category(category: Option<&str>) -> bool {
    category.is_some_and(|c| {
        c.starts_with("memory-") && c != kb_core::sessions::MEMORY_SESSION_CATEGORY
    })
}

/// A local twin of `echoes::session_title` (small pure helper, duplicated
/// per this route family's locality convention).
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
/// rule `sessions_list`'s own SQL enforces). A local twin of
/// `echoes::collapse_newest_capture` / `timeline::collapse_newest_capture`
/// (duplicated per this route family's locality convention), so the policy
/// is unit-testable without a DB here too.
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

/// Minimal HTML escape (`& < > "`) — mirrors the escape set used across kb
/// (`spa::escape_html_attr`, `meta_edit`, `iframe`): every title/label on this
/// page comes from operator-authored (but still untrusted-enough) artifact
/// metadata, so it is escaped before splicing into either a text node or a
/// `"`-quoted attribute.
fn escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            _ => out.push(c),
        }
    }
    out
}

/// Scale cap (in count units) for the activity bars — a day with more opens
/// than this just draws a full bar; the exact number is always printed
/// alongside it regardless, so nothing is lost, only the bar saturates.
const ACTIVITY_BAR_MAX: i64 = 20;
const ACTIVITY_BAR_PX_PER_UNIT: i64 = 9;
const ACTIVITY_BAR_MAX_PX: i64 = ACTIVITY_BAR_MAX * ACTIVITY_BAR_PX_PER_UNIT;

/// One inline SVG, three horizontal bars (opens/searches/comments). Black
/// fill on white, outlined at full scale — legible with zero colour
/// dependence, exactly the numbers `activity` carries (the bar is
/// supplementary; the printed count is the source of truth).
fn activity_svg(a: &DaycardActivity) -> String {
    let rows: [(&str, i64); 3] = [
        ("opens", a.opens),
        ("searches", a.searches),
        ("comments", a.comments),
    ];
    let row_h = 28;
    let mut body = String::new();
    for (i, (label, count)) in rows.iter().enumerate() {
        let y = i as i64 * row_h;
        let w = (*count).clamp(0, ACTIVITY_BAR_MAX) * ACTIVITY_BAR_PX_PER_UNIT;
        let text_y = y + 14;
        let bar_y = y + 2;
        body.push_str(&format!(
            "<text x=\"0\" y=\"{text_y}\" font-size=\"14\">{label}</text>\
<rect x=\"96\" y=\"{bar_y}\" width=\"{max_w}\" height=\"16\" fill=\"none\" stroke=\"#000\" stroke-width=\"1\"/>\
<rect x=\"96\" y=\"{bar_y}\" width=\"{w}\" height=\"16\" fill=\"#000\"/>\
<text x=\"{num_x}\" y=\"{text_y}\" font-size=\"14\">{count}</text>",
            max_w = ACTIVITY_BAR_MAX_PX,
            num_x = 96 + ACTIVITY_BAR_MAX_PX + 10,
        ));
    }
    format!(
        "<svg viewBox=\"0 0 260 {h}\" width=\"260\" height=\"{h}\" role=\"img\" \
aria-label=\"today's activity: {opens} opens, {searches} searches, {comments} comments\">{body}</svg>",
        h = rows.len() as i64 * row_h,
        opens = a.opens,
        searches = a.searches,
        comments = a.comments,
    )
}

const STYLE: &str = "<style>\
body{font-family:-apple-system,BlinkMacSystemFont,'Segoe UI',Helvetica,Arial,sans-serif;background:#fff;color:#000;margin:0;padding:28px;font-size:20px;line-height:1.35}\
header h1{font-size:28px;margin:0 0 2px}\
header .day{margin:0 0 8px;font-size:16px;color:#000}\
h2{font-size:17px;text-transform:uppercase;letter-spacing:.04em;border-bottom:2px solid #000;padding-bottom:4px;margin:26px 0 12px}\
ul{list-style:none;margin:0;padding:0}\
li{padding:8px 0;border-bottom:1px solid #000}\
li:last-child{border-bottom:none}\
.sub{font-size:15px;display:block;margin-top:2px}\
.empty{font-style:italic}\
svg text{fill:#000;font-family:-apple-system,BlinkMacSystemFont,'Segoe UI',Helvetica,Arial,sans-serif}\
</style>";

/// Render the full self-contained HTML+inline-SVG document. Pure — same
/// `DaycardResponse` in, same bytes out, every time (the determinism
/// contract; see the module doc + `render_html_is_deterministic`).
fn render_html(d: &DaycardResponse) -> String {
    let mut out = String::with_capacity(2048);
    out.push_str("<!doctype html>\n<html lang=\"en\">\n<head>\n");
    out.push_str("<meta charset=\"utf-8\">\n");
    out.push_str("<meta name=\"viewport\" content=\"width=device-width, initial-scale=1\">\n");
    out.push_str(&format!(
        "<title>kb daycard — {} — {}</title>\n",
        escape(&d.kb),
        d.day
    ));
    out.push_str(STYLE);
    out.push_str("\n</head>\n<body>\n");
    out.push_str(&format!(
        "<header><h1>{}</h1><p class=\"day\">{}</p></header>\n",
        escape(&d.kb),
        d.day
    ));

    out.push_str("<section id=\"activity\"><h2>Today</h2>\n");
    out.push_str(&activity_svg(&d.activity));
    out.push_str("\n</section>\n");

    out.push_str("<section id=\"resurface\"><h2>Worth picking back up</h2>\n");
    if d.resurface.is_empty() {
        out.push_str("<p class=\"empty\">Nothing waiting.</p>\n");
    } else {
        out.push_str("<ul>\n");
        for item in &d.resurface {
            let reason = item.reasons.first().map(String::as_str).unwrap_or("");
            out.push_str("<li><strong>");
            out.push_str(&escape(&item.title));
            out.push_str("</strong>");
            if !reason.is_empty() {
                out.push_str("<span class=\"sub\">");
                out.push_str(&escape(reason));
                out.push_str("</span>");
            }
            out.push_str("</li>\n");
        }
        out.push_str("</ul>\n");
    }
    out.push_str("</section>\n");

    out.push_str(&render_doc_section("Recently touched", &d.recent));
    out.push_str(&render_doc_section("Never opened", &d.never_opened));

    out.push_str("</body>\n</html>\n");
    out
}

fn render_doc_section(title: &str, docs: &[DaycardDoc]) -> String {
    let mut out = String::new();
    out.push_str("<section><h2>");
    out.push_str(&escape(title));
    out.push_str("</h2>\n");
    if docs.is_empty() {
        out.push_str("<p class=\"empty\">Nothing here.</p>\n");
    } else {
        out.push_str("<ul>\n");
        for doc in docs {
            out.push_str("<li>");
            out.push_str(&escape(&doc.title));
            out.push_str("</li>\n");
        }
        out.push_str("</ul>\n");
    }
    out.push_str("</section>\n");
    out
}

/// CT-E1 — render the `?since=` document. Same conventions as
/// [`render_html`] (system-font `STYLE`, no `<script>`/external assets,
/// honest empty states via `.empty`) — see the module doc.
fn render_since_html(d: &DaycardSinceResponse) -> String {
    let mut out = String::with_capacity(2048);
    out.push_str("<!doctype html>\n<html lang=\"en\">\n<head>\n");
    out.push_str("<meta charset=\"utf-8\">\n");
    out.push_str("<meta name=\"viewport\" content=\"width=device-width, initial-scale=1\">\n");
    out.push_str(&format!(
        "<title>kb daycard — {} — since {}</title>\n",
        escape(&d.kb),
        d.since_unix
    ));
    out.push_str(STYLE);
    out.push_str("\n</head>\n<body>\n");
    out.push_str(&format!(
        "<header><h1>{}</h1><p class=\"day\">while you were away: {} → {}</p></header>\n",
        escape(&d.kb),
        d.since_unix,
        d.to_unix
    ));

    out.push_str(&render_session_section(&d.sessions));
    out.push_str(&render_doc_section("Memories written", &d.memories));
    out.push_str(&render_doc_section(
        "Artifacts created / updated",
        &d.artifacts,
    ));
    out.push_str(&render_comment_section(&d.comments, d.comments_still_open));

    out.push_str("</body>\n</html>\n");
    out
}

fn render_session_section(items: &[DaycardSessionItem]) -> String {
    let mut out = String::new();
    out.push_str("<section><h2>Sessions</h2>\n");
    if items.is_empty() {
        out.push_str("<p class=\"empty\">Nothing here.</p>\n");
    } else {
        out.push_str("<ul>\n");
        for it in items {
            out.push_str("<li>");
            out.push_str(&escape(&it.title));
            out.push_str("</li>\n");
        }
        out.push_str("</ul>\n");
    }
    out.push_str("</section>\n");
    out
}

fn render_comment_section(items: &[DaycardCommentItem], still_open: u32) -> String {
    let mut out = String::new();
    out.push_str(&format!(
        "<section><h2>Comments raised ({still_open} still open)</h2>\n"
    ));
    if items.is_empty() {
        out.push_str("<p class=\"empty\">Nothing here.</p>\n");
    } else {
        out.push_str("<ul>\n");
        for it in items {
            out.push_str("<li>");
            out.push_str(&escape(&it.title));
            out.push_str(if it.open { " (open)" } else { " (resolved)" });
            out.push_str("</li>\n");
        }
        out.push_str("</ul>\n");
    }
    out.push_str("</section>\n");
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn doc(id: &str, title: &str, mtime: i64, category: Option<&str>) -> DocSummary {
        DocSummary {
            id: id.into(),
            title: title.into(),
            path: format!("/corpus/{id}.html"),
            kb_category: category.map(str::to_string),
            mtime_unix: Some(mtime),
            ..Default::default()
        }
    }

    // `pick_recent_and_never_opened` only checks rollup MEMBERSHIP (has this
    // artifact ever been opened at all?), never `.state`/`.completion_pct` —
    // so the test double just needs to exist, not be internally consistent.
    fn rollup_row(pct: u8) -> kb_core::reading::ReadRollup {
        kb_core::reading::ReadRollup {
            last_opened_unix: Some(1),
            completion_pct: pct,
            state: kb_core::lists::ReadState::InProgress,
        }
    }

    #[test]
    fn parse_day_accepts_calendar_date_only() {
        assert_eq!(
            parse_day("2026-07-26").unwrap(),
            chrono::NaiveDate::from_ymd_opt(2026, 7, 26).unwrap()
        );
        assert!(parse_day("1700000000").is_err());
        assert!(parse_day("garbage").is_err());
    }

    #[test]
    fn wants_json_prefers_explicit_format_param() {
        let headers = HeaderMap::new();
        let json_param = DaycardParams {
            day: None,
            since: None,
            format: Some("json".into()),
        };
        let html_param = DaycardParams {
            day: None,
            since: None,
            format: Some("html".into()),
        };
        assert!(wants_json(&headers, &json_param));
        assert!(!wants_json(&headers, &html_param));
    }

    #[test]
    fn wants_json_falls_back_to_accept_header_then_html_default() {
        let params = DaycardParams::default();
        let mut headers = HeaderMap::new();
        assert!(!wants_json(&headers, &params), "no header ⇒ HTML default");
        headers.insert(header::ACCEPT, "application/json".parse().unwrap());
        assert!(wants_json(&headers, &params));
        let mut headers = HeaderMap::new();
        headers.insert(
            header::ACCEPT,
            "text/html,application/xhtml+xml".parse().unwrap(),
        );
        assert!(!wants_json(&headers, &params));
    }

    #[test]
    fn pivot_activity_sums_and_drops_unknown_kinds() {
        let rows = vec![
            DayKindCount {
                day: "2026-07-01".into(),
                kind: "open".into(),
                count: 3,
            },
            DayKindCount {
                day: "2026-07-01".into(),
                kind: "search".into(),
                count: 2,
            },
            DayKindCount {
                day: "2026-07-01".into(),
                kind: "comment".into(),
                count: 1,
            },
            DayKindCount {
                day: "2026-07-01".into(),
                kind: "future-kind".into(),
                count: 99,
            },
        ];
        let a = pivot_activity(&rows);
        assert_eq!(a.opens, 3);
        assert_eq!(a.searches, 2);
        assert_eq!(a.comments, 1);
    }

    #[test]
    fn pivot_activity_empty_is_zero() {
        let a = pivot_activity(&[]);
        assert_eq!((a.opens, a.searches, a.comments), (0, 0, 0));
    }

    #[test]
    fn pick_recent_and_never_opened_excludes_memory_and_resurfaced_ids() {
        let docs = vec![
            doc("aaa", "A", 300, None),
            doc("bbb", "B", 200, None),
            doc("ccc", "C", 100, None),
            doc("mem", "Memory", 400, Some("memory-note")),
        ];
        let rollup: std::collections::HashMap<String, kb_core::reading::ReadRollup> =
            [("bbb".to_string(), rollup_row(10))].into_iter().collect();
        let exclude: HashSet<&str> = ["aaa"].into_iter().collect();
        let source = std::path::Path::new("/corpus");
        let (recent, never_opened) = pick_recent_and_never_opened(&docs, &rollup, &exclude, source);
        // aaa excluded (already resurfaced), mem excluded (memory category).
        assert_eq!(
            recent.iter().map(|d| d.id.as_str()).collect::<Vec<_>>(),
            vec!["bbb", "ccc"]
        );
        // bbb has a rollup entry (opened before) so it's not "never opened";
        // ccc has none and isn't claimed by `recent`... but ccc WAS claimed by
        // recent above, so never_opened is empty here — exactly the
        // no-repeat-across-sections contract.
        assert!(never_opened.is_empty());
    }

    #[test]
    fn pick_recent_and_never_opened_deterministic_tiebreak_on_id() {
        let docs = vec![doc("zzz", "Z", 100, None), doc("aaa", "A", 100, None)];
        let rollup = std::collections::HashMap::new();
        let exclude: HashSet<&str> = HashSet::new();
        let source = std::path::Path::new("/corpus");
        let (recent, _) = pick_recent_and_never_opened(&docs, &rollup, &exclude, source);
        assert_eq!(recent[0].id, "aaa");
        assert_eq!(recent[1].id, "zzz");
    }

    #[test]
    fn never_opened_finds_docs_beyond_the_recent_cap() {
        // 4 candidates, cap 3: the 4th (oldest by mtime) never claimed by
        // `recent` and absent from rollup should surface as never_opened.
        let docs = vec![
            doc("a", "A", 400, None),
            doc("b", "B", 300, None),
            doc("c", "C", 200, None),
            doc("d", "D", 100, None),
        ];
        let rollup = std::collections::HashMap::new();
        let exclude: HashSet<&str> = HashSet::new();
        let source = std::path::Path::new("/corpus");
        let (recent, never_opened) = pick_recent_and_never_opened(&docs, &rollup, &exclude, source);
        assert_eq!(recent.len(), DAYCARD_DOC_LIMIT);
        assert_eq!(never_opened.len(), 1);
        assert_eq!(never_opened[0].id, "d");
    }

    fn sample_response() -> DaycardResponse {
        DaycardResponse {
            kb: "kb".into(),
            day: "2026-07-26".into(),
            now_unix: 1_753_488_000,
            resurface: vec![],
            activity: DaycardActivity {
                opens: 4,
                searches: 1,
                comments: 0,
            },
            recent: vec![DaycardDoc {
                id: "abc".into(),
                title: "A & <B> \"quoted\"".into(),
                source_relative: "a.html".into(),
                mtime_unix: Some(1),
            }],
            never_opened: vec![],
        }
    }

    #[test]
    fn render_html_is_deterministic_for_same_input() {
        let a = render_html(&sample_response());
        let b = render_html(&sample_response());
        assert_eq!(a, b);
    }

    #[test]
    fn render_html_escapes_titles() {
        let html = render_html(&sample_response());
        assert!(html.contains("A &amp; &lt;B&gt; &quot;quoted&quot;"));
        assert!(!html.contains("<B>"));
    }

    #[test]
    fn render_html_has_no_script_and_no_external_resources() {
        let html = render_html(&sample_response());
        assert!(!html.contains("<script"));
        assert!(!html.contains("http://"));
        assert!(!html.contains("https://"));
        assert!(!html.contains("@font-face"));
    }

    #[test]
    fn render_html_honest_empty_states() {
        let mut r = sample_response();
        r.recent = vec![];
        r.never_opened = vec![];
        let html = render_html(&r);
        assert!(html.contains("Nothing waiting."));
        assert!(html.contains("Nothing here."));
    }

    #[test]
    fn render_html_never_mentions_gamification_terms() {
        let html = render_html(&sample_response());
        for banned in ["streak", "badge", "day 1", "🔥", "goal", "% complete"] {
            assert!(
                !html.to_lowercase().contains(&banned.to_lowercase()),
                "found banned term {banned:?}"
            );
        }
    }

    // ---- CT-E1: ?since= ------------------------------------------------

    fn session(id: &str, sid: &str, started_at: i64) -> SessionRow {
        SessionRow {
            artifact_id: id.into(),
            session_id: sid.into(),
            started_at,
            ended_at: started_at + 60,
            ..SessionRow::default()
        }
    }

    fn history_comment(artifact_id: &str, comment_id: &str, started_at: i64) -> HistoryRow {
        HistoryRow {
            id: 1,
            kind: "comment".into(),
            artifact_id: Some(artifact_id.into()),
            query: None,
            comment_id: Some(comment_id.into()),
            scroll_y: 0,
            scroll_max: 0,
            scroll_y_max: 0,
            started_at_unix: started_at,
            updated_at_unix: started_at,
            source: None,
            user: "operator".into(),
        }
    }

    fn open_item(comment_id: &str, artifact_id: &str, title: &str) -> InboxItem {
        InboxItem {
            kb: "kb".into(),
            artifact_id: artifact_id.into(),
            source_relative: Some(format!("{artifact_id}.html")),
            title: title.into(),
            comment_id: comment_id.into(),
            excerpt: String::new(),
            author: "you".into(),
            reply_count: 0,
            anchor: "file".into(),
            stale: false,
            created_at: 0,
            updated_at: 0,
        }
    }

    #[test]
    fn validate_day_since_exclusive_rejects_both_present() {
        assert!(validate_day_since_exclusive(&None, &None).is_ok());
        assert!(validate_day_since_exclusive(&Some("2026-07-26".into()), &None).is_ok());
        assert!(validate_day_since_exclusive(&None, &Some("3600".into())).is_ok());
        assert!(
            validate_day_since_exclusive(&Some("2026-07-26".into()), &Some("3600".into())).is_err()
        );
    }

    #[test]
    fn parse_since_bound_accepts_unix_seconds_and_calendar_date() {
        assert_eq!(parse_since_bound("1700000000").unwrap(), 1_700_000_000);
        assert_eq!(parse_since_bound("2024-01-01").unwrap(), 1_704_067_200);
        assert!(parse_since_bound("garbage").is_err());
    }

    #[test]
    fn filter_sessions_in_window_collapses_and_filters() {
        let rows = vec![
            session("cap-early", "s1", 100),
            session("cap-late", "s1", 200),
            session("out-of-window", "s2", 50),
        ];
        let (items, truncated) = filter_sessions_in_window(rows, 100, 300);
        assert_eq!(
            items
                .iter()
                .map(|i| i.session_id.as_str())
                .collect::<Vec<_>>(),
            vec!["s1"]
        );
        assert_eq!(items[0].started_at, 200);
        assert!(!truncated);
    }

    #[test]
    fn filter_sessions_in_window_empty_is_honest_zero() {
        let (items, truncated) = filter_sessions_in_window(vec![], 100, 300);
        assert!(items.is_empty());
        assert!(!truncated);
    }

    #[test]
    fn filter_sessions_in_window_truncates_and_flags() {
        let rows: Vec<SessionRow> = (0..(DAYCARD_SINCE_LANE_CAP + 5))
            .map(|i| session(&format!("a{i}"), &format!("s{i}"), 1_000 + i as i64))
            .collect();
        let (items, truncated) = filter_sessions_in_window(rows, 0, 10_000);
        assert_eq!(items.len(), DAYCARD_SINCE_LANE_CAP);
        assert!(truncated);
    }

    #[test]
    fn filter_docs_in_window_splits_memories_and_artifacts_by_window() {
        let docs = vec![
            doc("mem-in", "Memory in", 500, Some("memory-user")),
            doc("mem-out", "Memory out", 50, Some("memory-user")),
            doc("sess", "Session transcript", 500, Some("memory-session")),
            doc("art-in", "Artifact in", 500, None),
            doc("art-out", "Artifact out", 50, None),
        ];
        let source = std::path::Path::new("/corpus");
        let (memories, mem_trunc, artifacts, art_trunc) =
            filter_docs_in_window(&docs, 100, 900, source);
        assert_eq!(
            memories.iter().map(|d| d.id.as_str()).collect::<Vec<_>>(),
            vec!["mem-in"]
        );
        assert_eq!(
            artifacts.iter().map(|d| d.id.as_str()).collect::<Vec<_>>(),
            vec!["art-in"]
        );
        assert!(!mem_trunc);
        assert!(!art_trunc);
    }

    #[test]
    fn filter_docs_in_window_memory_uses_created_over_mtime() {
        // mtime is OUTSIDE the window; created_unix is INSIDE it — the
        // memories lane must key on created, never mtime (honesty rule).
        let mut d = doc("mem-created", "Created wins", 50, Some("memory-user"));
        d.created_unix = Some(500);
        let source = std::path::Path::new("/corpus");
        let (memories, _, _, _) = filter_docs_in_window(&[d], 100, 900, source);
        assert_eq!(memories.len(), 1);
        assert_eq!(memories[0].mtime_unix, Some(500));
    }

    #[test]
    fn filter_docs_in_window_memory_falls_back_to_mtime_when_no_created() {
        // created_unix absent (pre-v0.15 row, or a filesystem without
        // btime) — falls back to mtime_unix, which IS inside the window.
        let d = doc("mem-no-created", "No created", 500, Some("memory-user"));
        let source = std::path::Path::new("/corpus");
        let (memories, _, _, _) = filter_docs_in_window(&[d], 100, 900, source);
        assert_eq!(memories.len(), 1);
        assert_eq!(memories[0].mtime_unix, Some(500));
    }

    #[test]
    fn filter_docs_in_window_empty_window_is_honest_zero() {
        let docs = vec![doc("a", "A", 500, None)];
        let source = std::path::Path::new("/corpus");
        let (memories, _, artifacts, _) = filter_docs_in_window(&docs, 10_000, 20_000, source);
        assert!(memories.is_empty());
        assert!(artifacts.is_empty());
    }

    #[test]
    fn build_comment_items_marks_open_vs_resolved_and_counts_still_open() {
        let rows = vec![
            history_comment("art1", "c-open", 100),
            history_comment("art2", "c-resolved", 200),
        ];
        let open_items = vec![open_item("c-open", "art1", "Open thread")];
        let doc2 = doc("art2", "Resolved doc", 200, None);
        let docs_by_id: HashMap<&str, &DocSummary> = [("art2", &doc2)].into_iter().collect();
        let source = std::path::Path::new("/corpus");
        let (items, still_open) = build_comment_items(&rows, &open_items, &docs_by_id, source);
        assert_eq!(items.len(), 2);
        // newest raised_at first.
        assert_eq!(items[0].comment_id, "c-resolved");
        assert!(!items[0].open);
        assert_eq!(items[0].title, "Resolved doc");
        assert_eq!(items[1].comment_id, "c-open");
        assert!(items[1].open);
        assert_eq!(items[1].title, "Open thread");
        assert_eq!(still_open, 1);
    }

    #[test]
    fn build_comment_items_empty_is_honest_zero() {
        let docs_by_id: HashMap<&str, &DocSummary> = HashMap::new();
        let source = std::path::Path::new("/corpus");
        let (items, still_open) = build_comment_items(&[], &[], &docs_by_id, source);
        assert!(items.is_empty());
        assert_eq!(still_open, 0);
    }

    fn sample_since_response() -> DaycardSinceResponse {
        DaycardSinceResponse {
            kb: "kb".into(),
            since_unix: 1_753_400_000,
            to_unix: 1_753_488_000,
            sessions: vec![DaycardSessionItem {
                session_id: "s1".into(),
                title: "A & <B> \"quoted\"".into(),
                substance: Some("substantive".into()),
                started_at: 1_753_450_000,
            }],
            sessions_truncated: false,
            memories: vec![],
            memories_truncated: false,
            artifacts: vec![],
            artifacts_truncated: false,
            comments: vec![],
            comments_truncated: false,
            comments_still_open: 0,
        }
    }

    #[test]
    fn render_since_html_is_deterministic_for_same_input() {
        let a = render_since_html(&sample_since_response());
        let b = render_since_html(&sample_since_response());
        assert_eq!(a, b);
    }

    #[test]
    fn render_since_html_escapes_titles() {
        let html = render_since_html(&sample_since_response());
        assert!(html.contains("A &amp; &lt;B&gt; &quot;quoted&quot;"));
        assert!(!html.contains("<B>"));
    }

    #[test]
    fn render_since_html_has_no_script_and_no_external_resources() {
        let html = render_since_html(&sample_since_response());
        assert!(!html.contains("<script"));
        assert!(!html.contains("http://"));
        assert!(!html.contains("https://"));
        assert!(!html.contains("@font-face"));
    }

    #[test]
    fn render_since_html_honest_empty_states() {
        let mut r = sample_since_response();
        r.sessions = vec![];
        let html = render_since_html(&r);
        // All four lanes (sessions/memories/artifacts/comments) are empty —
        // "Nothing here." must appear once per lane, not be silently hidden.
        assert_eq!(html.matches("Nothing here.").count(), 4);
    }

    #[test]
    fn render_since_html_never_mentions_gamification_terms() {
        let html = render_since_html(&sample_since_response());
        for banned in ["streak", "badge", "day 1", "🔥", "goal", "% complete"] {
            assert!(
                !html.to_lowercase().contains(&banned.to_lowercase()),
                "found banned term {banned:?}"
            );
        }
    }
}
