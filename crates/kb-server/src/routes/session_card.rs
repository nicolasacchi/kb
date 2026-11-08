//! A3 — `GET /api/sessions/{session_id}/card` (kb-session-card/1) and
//! `GET /api/sessions/{session_id}/children`.
//!
//! The card is one in-process fan-out over the existing per-lane readers,
//! scoped to the session's NEWEST capture (invariant #11). Every lane
//! degrades on its own (drop-on-error, invariant #28): a failing reader
//! yields `available:false` + a reason and a `tracing::warn!`, never a
//! 500. The card is SURFACED, NEVER-SCORED — no ranking module reads it.
//! Neither this route nor the CLI ever carries transcript BODY or raw
//! turns (stored, capped, derived facts only), and there is no attach or
//! type surface for agents or the CLI.

use crate::middleware::{error_to_problem_json, Identity};
use crate::routes::sessions::{
    check_out, find_session, session_chain_parts, touched_files_for_commit, widen_session_ids,
    SessionOut,
};
use crate::state::KbHandles;
use axum::{
    extract::{ConnectInfo, Extension, Path, Query, State},
    http::HeaderMap,
    response::{IntoResponse, Response},
    Json,
};
use kb_core::sessions::card::*;
use kb_core::slate::SlateSlug;
use serde::{Deserialize, Serialize};
use std::net::SocketAddr;
use std::sync::Arc;

#[cfg(test)]
type SqliteRow = kb_core::storage::sqlite::SessionRow;
#[allow(dead_code)]
type CommitRow = kb_core::storage::sqlite::SessionCommitRow;
#[allow(dead_code)]
type FileRow = kb_core::storage::sqlite::SessionFileRow;
/// Fan-out future payloads, named so the future types stay under
/// `clippy::type_complexity` (same convention as `LocatedSession` in
/// `routes/sessions.rs`).
type ResolvedDoc = ((String, String), (String, String));
type ResolvedTouched = (String, Vec<kb_core::vcs::TouchedFile>);

const CARD_FILES_CAP: usize = 100;
const CARD_DECISIONS_CAP: usize = 50;
const CARD_COMMITS_CAP: usize = 50;
const CARD_RESEARCH_CAP: usize = 20;
const CARD_COMMENTS_CAP: usize = 20;
const CARD_READINGS_CAP: usize = 20;
const CARD_SLATE_CAP: usize = 20;
const CARD_CHILDREN_CAP: u32 = 50;
const CARD_PRODUCED_CAP: usize = 50;
const HISTORY_SCAN_CAP: u32 = 500;
const RAISED_SCAN_CAP: u32 = 1000;
const RAISED_WINDOW_FALLBACK_SECS: i64 = 86_400;
const PER_SESSION_MEMORY_LIMIT: u32 = 100;

// ---------------------------------------------------------------------------
// Children: one predicate helper for the card lane AND /children.
// ---------------------------------------------------------------------------

/// The per-lane degrade rule (brief S1 / #28 drop-on-error): a federated
/// reader family that failed in EVERY kb has no source of truth left —
/// the lane must report `available:false` with a reason, never
/// `available:true` with empty/zero invented data. A family that
/// answered in at least one kb keeps the lane available with that data
/// (partial is honest; all-dark is a lie). Zero kbs is vacuously
/// available: there was no reader to fail.
#[derive(Debug, Clone)]
enum ChildrenPredicate {
    /// The whole subagent flood under a root (any depth).
    Root(String),
    /// A non-root session's direct children.
    Parent(String),
}

/// The degrade decision, as a pure rule: `failures`/`total` are the
/// per-kb read outcomes of one reader family of one lane.
fn reader_family_dark(failures: usize, total: usize) -> bool {
    total > 0 && failures == total
}

impl ChildrenPredicate {
    fn args(&self) -> (Option<String>, Option<String>) {
        match self {
            ChildrenPredicate::Root(id) => (Some(id.clone()), None),
            ChildrenPredicate::Parent(id) => (None, Some(id.clone())),
        }
    }
}

/// ONE helper decides the children predicate, used by the card and by
/// `/children`: if the target row's own `root_session_id IS NULL` it is a
/// root and its children are the newest-capture rows with
/// `root_session_id = sid` (the whole flood, any depth); otherwise
/// children are rows with `parent_session_id = sid`. Depth lives in
/// `parent_session_id`; `root_session_id` groups the flood.
fn children_predicate(target: &SessionOut) -> ChildrenPredicate {
    if target.root_session_id.is_none() {
        ChildrenPredicate::Root(target.session_id.clone())
    } else {
        ChildrenPredicate::Parent(target.session_id.clone())
    }
}

/// Federated children total as a UNION of child session ids across kbs
/// (held in [`children_total_from`]). Returns `Err` only when every kb's
/// reader failed, so the summary/children lanes can degrade honestly
/// (`available:false`) instead of reporting 0. A partial failure keeps the
/// healthy kbs' ids: at least one source of truth answered.
async fn children_total(
    state: &Arc<KbHandles>,
    pred: &ChildrenPredicate,
) -> Result<u64, kb_core::Error> {
    let sources: Vec<(String, kb_core::storage::StorageHandle)> = state
        .kbs
        .iter()
        .map(|(name, ctx)| (name.as_str().to_string(), ctx.storage.clone()))
        .collect();
    children_total_from(&sources, state.fanout_cap, pred).await
}

/// The total's testable core: over `(kb name, storage)` sources, report
/// how many DISTINCT child session ids the predicate matches anywhere.
/// Per-kb failure folds to no rows with a warn (never a 500); if every
/// source failed the whole call is `Err` so callers degrade rather than
/// invent a total. A session id captured in two kbs (the `commit_files`
/// fan-out posture — V0040's UNIQUE index is per-DB, not global)
/// counts once: ids are unioned by session id, first wins.
async fn children_total_from(
    sources: &[(String, kb_core::storage::StorageHandle)],
    fanout_cap: usize,
    pred: &ChildrenPredicate,
) -> Result<u64, kb_core::Error> {
    let (root_of, parent_of) = pred.args();
    let mut futs: Vec<super::CorpusFut<'_, Result<Vec<String>, kb_core::Error>>> = Vec::new();
    for (kb_name, storage) in sources {
        let kb_name = kb_name.clone();
        let storage = storage.clone();
        let root_of = root_of.clone();
        let parent_of = parent_of.clone();
        futs.push(Box::pin(async move {
            match storage.sessions_children_ids(root_of, parent_of).await {
                Ok(ids) => Ok(ids),
                Err(e) => {
                    tracing::warn!(kb = %kb_name, error = %e, "sessions_children_ids failed");
                    Err(kb_core::Error::Storage(format!("{kb_name}: {e}")))
                }
            }
        }));
    }
    let joined = super::buffered_join(futs, fanout_cap).await;
    if !joined.is_empty() && joined.iter().all(|r| r.is_err()) {
        return Err(kb_core::Error::Storage(
            "sessions children unreadable in every kb".to_string(),
        ));
    }
    let mut ids: std::collections::HashSet<String> = std::collections::HashSet::new();
    for r in joined.into_iter().flatten() {
        for id in r {
            ids.insert(id);
        }
    }
    Ok(ids.len() as u64)
}

/// Federated one-page children fetch: over-fetches `limit + 1` per kb,
/// then merges by `(started_at, session_id)` and truncates. `after` is
/// `(started_at, session_id)`; the caller derives `next_after` from the
/// overflow. A session id captured in two kbs (the `commit_files`
/// fan-out posture: V0040's UNIQUE index is per-DB, not global) is kept
/// once, first kb wins, so the page never shows the same child twice and
/// stays consistent with [`children_total`]'s id union (and the
/// `children_total_matches_card_children_total` pin).
async fn children_page(
    state: &Arc<KbHandles>,
    pred: &ChildrenPredicate,
    after: Option<(i64, &str)>,
    limit: u32,
) -> Vec<SessionOut> {
    let (root_of, parent_of) = pred.args();
    let fetch_limit = limit.saturating_add(1);
    let mut futs: Vec<super::CorpusFut<'_, Vec<SessionOut>>> = Vec::new();
    for (kb_name, ctx) in state.kbs.iter() {
        let root_of = root_of.clone();
        let parent_of = parent_of.clone();
        let after: Option<(i64, String)> = after.map(|(a, s)| (a, s.to_string()));
        futs.push(Box::pin(async move {
            match ctx
                .storage
                .sessions_children_page(
                    root_of,
                    parent_of,
                    after.as_ref().map(|(a, s)| (*a, s.clone())),
                    fetch_limit,
                )
                .await
            {
                Ok(r) => r
                    .into_iter()
                    .map(|row| SessionOut::from_row(kb_name.as_str(), row))
                    .collect(),
                Err(e) => {
                    tracing::warn!(kb = %kb_name, error = %e, "sessions_children_page failed");
                    Vec::new()
                }
            }
        }));
    }
    // First-wins dedup by session id: the same child captured in two
    // kbs arrives twice (per-kb `is_newest` cannot see across DBs);
    // keep the first kb's row (kbs iterate in name order).
    let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
    let mut rows: Vec<SessionOut> = Vec::new();
    for out in super::buffered_join(futs, state.fanout_cap)
        .await
        .into_iter()
        .flatten()
    {
        if seen.insert(out.session_id.clone()) {
            rows.push(out);
        }
    }
    rows.sort_by(|a, b| {
        a.started_at
            .cmp(&b.started_at)
            .then_with(|| a.session_id.cmp(&b.session_id))
    });
    rows.truncate(fetch_limit as usize);
    rows
}

fn child_row(out: &SessionOut) -> ChildRow {
    ChildRow {
        session_id: out.session_id.clone(),
        display_name: out.display_name.clone(),
        started_at: out.started_at,
        ended_at: out.ended_at,
        harness: out.harness.clone(),
        substance: out.substance.clone(),
        files_edited_count: out.files_edited_count,
        tool_calls: out.tool_calls,
        parent_session_id: out.parent_session_id.clone(),
    }
}

/// `"<started_at>:<session_id>"`; session ids may contain `:`.
fn encode_after(out: &SessionOut) -> String {
    format!("{}:{}", out.started_at, out.session_id)
}

/// Split on the FIRST `:` — `started_at` is an integer, so everything
/// after it is the session id even when that id contains colons.
fn parse_after(raw: &str) -> Result<(i64, &str), kb_core::Error> {
    let bad = || kb_core::Error::BadRequest(format!("malformed children cursor {raw:?}"));
    let (at, sid) = raw.split_once(':').ok_or_else(bad)?;
    let at = at.parse::<i64>().map_err(|_| bad())?;
    if sid.is_empty() {
        return Err(bad());
    }
    Ok((at, sid))
}

#[derive(Debug, Deserialize, Default)]
pub struct ChildrenQuery {
    after: Option<String>,
    limit: Option<u32>,
}

#[derive(Debug, Serialize)]
struct ChildrenResponse {
    pub session_id: String,
    pub total: u64,
    pub items: Vec<ChildRow>,
    pub next_after: Option<String>,
}

/// `GET /api/sessions/{session_id}/children?after=&limit=` — paged
/// children of one session, oldest first by `(started_at, session_id)`.
/// `limit` defaults to 50, clamps to 1..=200. Malformed `after` is a 400.
/// 404 when no kb holds the session row.
pub async fn children(
    State(state): State<Arc<KbHandles>>,
    Path(session_id): Path<String>,
    Query(q): Query<ChildrenQuery>,
) -> Response {
    let limit = q.limit.unwrap_or(CARD_CHILDREN_CAP).clamp(1, 200);
    let mut after: Option<(i64, String)> = None;
    if let Some(raw) = q.after.as_deref() {
        match parse_after(raw) {
            Ok((at, sid)) => after = Some((at, sid.to_string())),
            Err(e) => return error_to_problem_json(&e),
        }
    }
    let Some(target) = find_session(&state, &session_id).await else {
        return error_to_problem_json(&kb_core::Error::NotFound(format!("session {session_id}")));
    };
    let pred = children_predicate(&target);
    let (total, page) = tokio::join!(
        children_total(&state, &pred),
        children_page(
            &state,
            &pred,
            after.as_ref().map(|(a, s)| (*a, s.as_str())),
            limit
        )
    );
    // The paged route has no `available` shape to degrade into (unlike the
    // card lanes): when every kb's children reader failed, the honest
    // answer is a storage-failure problem+json — never a fabricated zero
    // total beside a page drained from the same dead readers.
    let Ok(total) = total else {
        return error_to_problem_json(&kb_core::Error::Storage(
            "sessions children unreadable".to_string(),
        ));
    };
    let (items, next_after) = page_overfetch_into(page, limit);
    Json(ChildrenResponse {
        session_id: target.session_id.clone(),
        total,
        items: items.iter().map(child_row).collect(),
        next_after,
    })
    .into_response()
}

/// Split an over-fetched page at the requested `limit`.
fn page_overfetch_into(page: Vec<SessionOut>, limit: u32) -> (Vec<SessionOut>, Option<String>) {
    if page.len() as u32 > limit {
        let mut head: Vec<SessionOut> = page.into_iter().take(limit as usize).collect();
        let next_after = head.last().map(encode_after);
        let items = std::mem::take(&mut head);
        (items, next_after)
    } else {
        (page, None)
    }
}

// ---------------------------------------------------------------------------
// Card lanes — each its own future, each degrading on its own.
// ---------------------------------------------------------------------------

fn session_ctx<'a>(state: &'a Arc<KbHandles>, kb: &str) -> Option<&'a crate::state::KbContext> {
    let kb_name = kb_core::types::KbName::new(kb).ok()?;
    state.kbs.get(&kb_name)
}

fn action_bucket(action: &str) -> u8 {
    match action {
        "read" => 0,
        "edit" => 1,
        "write" => 2,
        _ => 0,
    }
}

async fn files_lane(
    state: &Arc<KbHandles>,
    session: &SessionOut,
    session_id: &str,
) -> (FilesLane, Vec<FileRow>, bool) {
    let Some(ctx) = session_ctx(state, &session.kb) else {
        return (FilesLane::unavailable("unknown kb"), Vec::new(), false);
    };
    match ctx
        .storage
        .session_files_for_session(session_id.to_string())
        .await
    {
        Ok(rows) => {
            let mut items: Vec<FileItem> = rows
                .iter()
                .map(|r| FileItem {
                    path: r.path.clone(),
                    action: r.action.clone(),
                    via_subagent: r.via_subagent,
                })
                .collect();
            items.sort_by(|a, b| a.path.cmp(&b.path));
            let total = items.len() as u32;
            let (mut read, mut edited, mut wrote) = (0u32, 0u32, 0u32);
            for it in &items {
                match action_bucket(&it.action) {
                    1 => edited += 1,
                    2 => wrote += 1,
                    _ => read += 1,
                }
            }
            (
                FilesLane {
                    available: true,
                    reason: None,
                    files: items.into_iter().take(CARD_FILES_CAP).collect(),
                    total,
                    read,
                    edited,
                    wrote,
                },
                rows,
                true,
            )
        }
        Err(e) => {
            tracing::warn!(kb = %session.kb, error = %e, "session_files_for_session failed in card");
            (
                FilesLane::unavailable("files unreadable"),
                Vec::new(),
                false,
            )
        }
    }
}

async fn decisions_lane(
    state: &Arc<KbHandles>,
    session: &SessionOut,
    session_id: &str,
) -> DecisionsLane {
    let Some(ctx) = session_ctx(state, &session.kb) else {
        return DecisionsLane::unavailable("unknown kb");
    };
    match ctx
        .storage
        .session_decisions_for_session(session_id.to_string())
        .await
    {
        Ok(rows) => {
            let total = rows.len() as u32;
            DecisionsLane {
                available: true,
                reason: None,
                decisions: rows
                    .into_iter()
                    .take(CARD_DECISIONS_CAP)
                    .map(|r| DecisionItem {
                        kind: r.kind,
                        prompt: r.prompt,
                        answer: r.answer,
                    })
                    .collect(),
                total,
            }
        }
        Err(e) => {
            tracing::warn!(kb = %session.kb, error = %e, "session_decisions_for_session failed in card");
            DecisionsLane::unavailable("decisions unreadable")
        }
    }
}

async fn commits_lane(
    state: &Arc<KbHandles>,
    session: &SessionOut,
    session_id: &str,
) -> (CommitsLane, Vec<CommitRow>, bool) {
    let Some(ctx) = session_ctx(state, &session.kb) else {
        return (CommitsLane::unavailable("unknown kb"), Vec::new(), false);
    };
    match ctx
        .storage
        .session_commits_for_session(session_id.to_string())
        .await
    {
        Ok(rows) => {
            let total = rows.len() as u32;
            (
                CommitsLane {
                    available: true,
                    reason: None,
                    commits: rows
                        .iter()
                        .take(CARD_COMMITS_CAP)
                        .map(|r| CommitItem {
                            kind: r.kind.clone(),
                            sha: r.sha.clone(),
                            subject: r.subject.clone(),
                            resolved: r.resolved,
                        })
                        .collect(),
                    total,
                },
                rows,
                true,
            )
        }
        Err(e) => {
            tracing::warn!(kb = %session.kb, error = %e, "session_commits_for_session failed in card");
            (
                CommitsLane::unavailable("commits unreadable"),
                Vec::new(),
                false,
            )
        }
    }
}

async fn research_lane(state: &Arc<KbHandles>, session_id: &str) -> ResearchLane {
    let session_id = session_id.to_string();
    let mut futs: Vec<
        super::CorpusFut<
            '_,
            Result<Vec<kb_core::storage::sqlite::SessionResearchRow>, kb_core::Error>,
        >,
    > = Vec::new();
    for (kb_name, ctx) in state.kbs.iter() {
        let session_id = session_id.clone();
        futs.push(Box::pin(async move {
            match ctx
                .storage
                .session_research_for_session(session_id)
                .await
            {
                Ok(v) => Ok(v),
                Err(e) => {
                    tracing::warn!(kb = %kb_name, error = %e, "session_research_for_session failed in card");
                    Err(kb_core::Error::Storage(format!("{kb_name}: {e}")))
                }
            }
        }));
    }
    let joined = super::buffered_join(futs, state.fanout_cap).await;
    let failures = joined.iter().filter(|r| r.is_err()).count();
    if reader_family_dark(failures, joined.len()) {
        return ResearchLane::unavailable("research unreadable");
    }
    let mut rows: Vec<kb_core::storage::sqlite::SessionResearchRow> =
        joined.into_iter().flatten().flatten().collect();
    // Explicit total order (seq, query, kind): cross-kb seq ties resolve
    // without depending on storage walk order.
    rows.sort_by(|a, b| {
        a.seq
            .cmp(&b.seq)
            .then_with(|| a.query.cmp(&b.query))
            .then_with(|| a.kind.cmp(&b.kind))
    });
    let total = rows.len() as u32;
    let mut kind_counts: std::collections::BTreeMap<String, u32> =
        std::collections::BTreeMap::new();
    for r in &rows {
        *kind_counts.entry(r.kind.clone()).or_insert(0) += 1;
    }
    ResearchLane {
        available: true,
        reason: None,
        kind_counts,
        items: rows
            .into_iter()
            .take(CARD_RESEARCH_CAP)
            .map(|r| ResearchItem {
                kind: r.kind,
                query: r.query,
            })
            .collect(),
        total,
    }
}

/// R5 reader behind `session_comments`: open comments on the in-corpus
/// artifacts this session touched, plus what the session raised during
/// its window — the handler's two projections. The touched set mirrors
/// the handler's scope exactly: `session_files_for_session` fans out
/// across ALL kbs (a session may be captured across corpora), not just
/// the session's own kb; a per-kb failure folds to no rows with a warn.
async fn comments_lane(state: &Arc<KbHandles>, session: &SessionOut) -> CommentsLane {
    let sid = session.session_id.clone();
    let mut file_futs: Vec<
        super::CorpusFut<'_, Result<Vec<kb_core::storage::sqlite::SessionFileRow>, kb_core::Error>>,
    > = Vec::new();
    for (kb_name, ctx) in state.kbs.iter() {
        let sid = sid.clone();
        file_futs.push(Box::pin(async move {
            match ctx
                .storage
                .session_files_for_session(sid)
                .await
            {
                Ok(v) => Ok(v),
                Err(e) => {
                    tracing::warn!(kb = %kb_name, error = %e, "session_files_for_session failed in card comments");
                    Err(kb_core::Error::Storage(format!("{kb_name}: {e}")))
                }
            }
        }));
    }
    let file_joined = super::buffered_join(file_futs, state.fanout_cap).await;
    let file_failures = file_joined.iter().filter(|r| r.is_err()).count();
    if reader_family_dark(file_failures, file_joined.len()) {
        return CommentsLane::unavailable("comments unreadable");
    }
    let rows: Vec<kb_core::storage::sqlite::SessionFileRow> =
        file_joined.into_iter().flatten().flatten().collect();
    let touched: std::collections::HashSet<(String, String)> = rows
        .iter()
        .filter(|r| r.in_corpus)
        .filter_map(|r| Some((r.target_kb.clone()?, r.target_artifact_id.clone()?)))
        .collect();
    let mut by_kb: std::collections::BTreeMap<String, std::collections::BTreeSet<String>> =
        std::collections::BTreeMap::new();
    for (kb, aid) in &touched {
        by_kb.entry(kb.clone()).or_default().insert(aid.clone());
    }
    let by_kb = &by_kb;
    let mut futs: Vec<super::CorpusFut<'_, Vec<CommentItem>>> = Vec::new();
    for (kb_name, kb_ctx) in state.kbs.iter() {
        let review_dir = state.paths.kb_review_dir(kb_name);
        futs.push(Box::pin(async move {
            let Some(ids) = by_kb.get(kb_name.as_str()) else {
                return Vec::new();
            };
            let mut out = Vec::new();
            for aid in ids {
                let path = review_dir.join(format!("{aid}.json"));
                let Ok(Some(file)) = kb_core::review::load(&path) else {
                    continue;
                };
                let title = kb_ctx
                    .storage
                    .get_by_id(aid.clone())
                    .await
                    .ok()
                    .flatten()
                    .map(|d| d.title)
                    .unwrap_or_else(|| file.artifact.title.clone());
                for c in file.visible(kb_core::review::Visibility::Public) {
                    if c.status != kb_core::review::CommentStatus::Open {
                        continue;
                    }
                    out.push(CommentItem {
                        comment_id: c.id.clone(),
                        kb: kb_name.as_str().to_string(),
                        artifact_id: aid.clone(),
                        title: Some(title.clone()),
                        author: match c.author {
                            kb_core::review::Author::You => "you",
                            kb_core::review::Author::Claude => "claude",
                        }
                        .to_string(),
                        body: c.body.clone(),
                        file_label: c.file_label.clone(),
                        created_at: c.created_at.timestamp(),
                        status: "open".to_string(),
                        touched: true,
                    });
                }
            }
            out
        }));
    }
    let mut open: Vec<CommentItem> = super::buffered_join(futs, state.fanout_cap)
        .await
        .into_iter()
        .flatten()
        .collect();
    // Explicit total order (kb, artifact_id, comment_id) so repeat reads
    // are byte-identical by construction, not by storage walk order.
    open.sort_by(|a, b| {
        a.kb.cmp(&b.kb)
            .then_with(|| a.artifact_id.cmp(&b.artifact_id))
            .then_with(|| a.comment_id.cmp(&b.comment_id))
    });
    let open_count = open.len() as u32;

    let raised_from = session.started_at;
    let raised_to = if session.ended_at > session.started_at {
        session.ended_at
    } else {
        session.started_at + RAISED_WINDOW_FALLBACK_SECS
    };
    let touched_ref = &touched;
    let mut raised_futs: Vec<super::CorpusFut<'_, Result<Vec<CommentItem>, kb_core::Error>>> =
        Vec::new();
    for (kb_name, kb_ctx) in state.kbs.iter() {
        let review_dir = state.paths.kb_review_dir(kb_name);
        let kb_name = kb_name.clone();
        raised_futs.push(Box::pin(async move {
            let rows = match kb_ctx
                .storage
                .history_comments_in_window(raised_from, raised_to, RAISED_SCAN_CAP)
                .await
            {
                Ok(v) => v,
                Err(e) => {
                    tracing::warn!(kb = %kb_name, error = %e, "history_comments_in_window failed in card");
                    return Err(kb_core::Error::Storage(format!("{kb_name}: {e}")));
                }
            };
            let mut out = Vec::new();
            let mut review_cache: std::collections::HashMap<String, Option<kb_core::review::ReviewFile>> =
                std::collections::HashMap::new();
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
                // LEAK GUARD (agent-facing bodies) — a `history` row can
                // survive a public→private flip only if its delete failed,
                // so a private comment is dropped whole here, not just its
                // body (the title row is an existence leak too).
                if c.is_private() {
                    continue;
                }
                let title = Some(if let Some(t) = title_cache.get(&aid) {
                    t.clone()
                } else {
                    let t = match kb_ctx.storage.get_by_id(aid.clone()).await {
                        Ok(Some(d)) => d.title,
                        _ => file.artifact.title.clone(),
                    };
                    title_cache.insert(aid.clone(), t.clone());
                    t
                });
                out.push(CommentItem {
                    comment_id: cid.clone(),
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
            Ok(out)
        }));
    }
    let raised_joined = super::buffered_join(raised_futs, state.fanout_cap).await;
    let raised_failures = raised_joined.iter().filter(|r| r.is_err()).count();
    if reader_family_dark(raised_failures, raised_joined.len()) {
        return CommentsLane::unavailable("comments unreadable");
    }
    let mut raised: Vec<CommentItem> = raised_joined.into_iter().flatten().flatten().collect();
    raised.sort_by(|a, b| {
        b.created_at
            .cmp(&a.created_at)
            .then_with(|| a.kb.cmp(&b.kb))
            .then_with(|| a.comment_id.cmp(&b.comment_id))
    });
    let raised_count = raised.len() as u32;
    CommentsLane {
        available: true,
        reason: None,
        open_count,
        raised_count,
        open: open.into_iter().take(CARD_COMMENTS_CAP).collect(),
        raised: raised.into_iter().take(CARD_COMMENTS_CAP).collect(),
    }
}

/// RP reader behind `readings`: history opens in the session window,
/// newest-first, deduped per artifact (newest kept), first 20 plus total.
async fn readings_lane(state: &Arc<KbHandles>, session: &SessionOut) -> ReadingsLane {
    let (from, to) = (session.started_at, session.ended_at);
    let operator = state.operator_user().to_string();
    let mut futs: Vec<super::CorpusFut<'_, Result<Vec<ReadingItem>, kb_core::Error>>> = Vec::new();
    for (kb_name, ctx) in state.kbs.iter() {
        let operator = operator.clone();
        futs.push(Box::pin(async move {
            let opens = match ctx
                .storage
                .history_opens_in_window(from, to, HISTORY_SCAN_CAP)
                .await
            {
                Ok(v) => v,
                Err(e) => {
                    tracing::warn!(kb = %kb_name, error = %e, "history_opens_in_window failed in card");
                    return Err(kb_core::Error::Storage(format!("{kb_name}: {e}")));
                }
            };
            let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
            let mut rows: Vec<ReadingItem> = Vec::new();
            for row in opens {
                let Some(artifact_id) = row.artifact_id else {
                    continue;
                };
                if !seen.insert(artifact_id.clone()) {
                    continue;
                }
                let (title, source_relative) = match ctx.storage.get_by_id(artifact_id.clone()).await
                {
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
                        Ok(Some((pct, last, _))) => (Some(f64::from(pct)), last),
                        _ => (None, None),
                    }
                } else {
                    (None, None)
                };
                rows.push(ReadingItem {
                    kb: kb_name.as_str().to_string(),
                    artifact_id,
                    title,
                    source_relative,
                    opened_at: row.started_at_unix,
                    read_pct,
                    last_section,
                });
            }
            Ok(rows)
        }));
    }
    let joined = super::buffered_join(futs, state.fanout_cap).await;
    let failures = joined.iter().filter(|r| r.is_err()).count();
    if reader_family_dark(failures, joined.len()) {
        return ReadingsLane::unavailable("readings unreadable");
    }
    let mut rows: Vec<ReadingItem> = joined.into_iter().flatten().flatten().collect();
    // Explicit total order (opened_at desc, artifact_id, kb): same-second
    // opens resolve without depending on storage walk order.
    rows.sort_by(|a, b| {
        b.opened_at
            .cmp(&a.opened_at)
            .then_with(|| a.artifact_id.cmp(&b.artifact_id))
            .then_with(|| a.kb.cmp(&b.kb))
    });
    let total = rows.len() as u32;
    ReadingsLane {
        available: true,
        reason: None,
        items: rows.into_iter().take(CARD_READINGS_CAP).collect(),
        total,
    }
}

/// Memory produced + recalled. Produced uses the same
/// `list_docs_with_kb_session` fan-out `get` uses (sorted, deduped,
/// cap 50 + total). Recalled uses the `recalls` handler's session-kb
/// ledger scope, with each hit's MR1 `pos` preserved (NULL stays NULL,
/// never inferred).
async fn memory_lane(
    state: &Arc<KbHandles>,
    session: &SessionOut,
    read_ids: &[String],
) -> MemoryLane {
    let mut futs: Vec<super::CorpusFut<'_, Result<Vec<String>, kb_core::Error>>> = Vec::new();
    for (kb_name, ctx) in state.kbs.iter() {
        let read_ids = read_ids.to_vec();
        let kb_name = kb_name.clone();
        futs.push(Box::pin(async move {
            let mut id_futs: Vec<super::CorpusFut<'_, Result<Vec<String>, kb_core::Error>>> = Vec::new();
            for sid in &read_ids {
                let sid = sid.clone();
                id_futs.push(Box::pin(async move {
                    match ctx
                        .storage
                        .list_docs_with_kb_session(sid, PER_SESSION_MEMORY_LIMIT)
                        .await
                    {
                        Ok(rows) => Ok(rows.into_iter().map(|r| r.id).collect::<Vec<_>>()),
                        Err(e) => {
                            tracing::warn!(kb = %kb_name, error = %e, "list_docs_with_kb_session failed in card");
                            Err(kb_core::Error::Storage(format!("{kb_name}: {e}")))
                        }
                    }
                }));
            }
            let joined = super::buffered_join(id_futs, state.fanout_cap).await;
            // This kb fails the produced family only when every one of its
            // reads failed (an empty read-id set is not a failure).
            if !read_ids.is_empty()
                && !joined.is_empty()
                && joined.iter().all(|r| r.is_err())
            {
                return Err(
                    joined
                        .into_iter()
                        .find_map(|r| r.err())
                        .unwrap(),
                );
            }
            let mut ids: Vec<String> = Vec::new();
            for v in joined.into_iter().flatten() {
                ids.extend(v);
            }
            Ok(ids)
        }));
    }
    let produced_joined = super::buffered_join(futs, state.fanout_cap).await;
    let produced_failures = produced_joined.iter().filter(|r| r.is_err()).count();
    if reader_family_dark(produced_failures, produced_joined.len()) {
        return MemoryLane::unavailable("memory unreadable");
    }
    let mut produced: Vec<String> = produced_joined.into_iter().flatten().flatten().collect();
    produced.sort();
    produced.dedup();
    let produced_total = produced.len() as u32;
    let produced: Vec<String> = produced.into_iter().take(CARD_PRODUCED_CAP).collect();

    let Some(ctx) = session_ctx(state, &session.kb) else {
        return MemoryLane::unavailable("unknown kb");
    };
    let mut rows = Vec::new();
    let mut id_futs: Vec<
        super::CorpusFut<
            '_,
            Result<Vec<kb_core::storage::sqlite::MemoryRecallRow>, kb_core::Error>,
        >,
    > = Vec::new();
    for sid in read_ids {
        let sid = sid.clone();
        let session_kb = session.kb.clone();
        id_futs.push(Box::pin(async move {
            match ctx.storage.memory_recalls_for_session(sid).await {
                Ok(v) => Ok(v),
                Err(e) => {
                    tracing::warn!(kb = %session_kb, error = %e, "memory_recalls_for_session failed in card");
                    Err(kb_core::Error::Storage(format!("{session_kb}: {e}")))
                }
            }
        }));
    }
    let recalled_joined = super::buffered_join(id_futs, state.fanout_cap).await;
    let recalled_failures = recalled_joined.iter().filter(|r| r.is_err()).count();
    if !read_ids.is_empty() && reader_family_dark(recalled_failures, recalled_joined.len()) {
        return MemoryLane::unavailable("memory unreadable");
    }
    for v in recalled_joined.into_iter().flatten() {
        rows.extend(v);
    }
    // Explicit total order (recalled_at with NULL last, memory_id,
    // memory_kb) on every read, not just multi-part chains: same-instant
    // recalls resolve without depending on row walk order.
    rows.sort_by(|a, b| {
        a.recalled_at
            .is_none()
            .cmp(&b.recalled_at.is_none())
            .then_with(|| a.recalled_at.cmp(&b.recalled_at))
            .then_with(|| a.memory_id.cmp(&b.memory_id))
            .then_with(|| a.memory_kb.cmp(&b.memory_kb))
    });
    let mut ids_by_kb: std::collections::BTreeMap<String, Vec<String>> =
        std::collections::BTreeMap::new();
    for r in &rows {
        ids_by_kb
            .entry(r.memory_kb.clone())
            .or_default()
            .push(r.memory_id.clone());
    }
    let mut resolved: std::collections::HashMap<(String, String), (String, String)> =
        std::collections::HashMap::new();
    let mut resolve_futs: Vec<super::CorpusFut<'_, Vec<ResolvedDoc>>> = Vec::new();
    for (memory_kb, ids) in ids_by_kb {
        let Some(mem_ctx) = session_ctx(state, &memory_kb) else {
            continue;
        };
        resolve_futs.push(Box::pin(async move {
            let docs = mem_ctx.storage.get_by_ids(ids).await.unwrap_or_default();
            let mut out = Vec::new();
            for d in docs {
                let source_relative = kb_core::paths::doc_rel_path(&d.path, &mem_ctx.source_path);
                out.push((
                    (memory_kb.clone(), d.id.clone()),
                    (d.title, source_relative),
                ));
            }
            out
        }));
    }
    for v in super::buffered_join(resolve_futs, state.fanout_cap).await {
        for (k, val) in v {
            resolved.insert(k, val);
        }
    }
    let recalled = rows
        .into_iter()
        .map(|r| {
            let display = resolved.get(&(r.memory_kb.clone(), r.memory_id.clone()));
            RecallItem {
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
    MemoryLane {
        available: true,
        reason: None,
        produced,
        produced_total,
        recalled,
    }
}

/// Pure half of [`summary_lane`]: the lane for a resolved children total.
/// `Err` (every kb's children reader failed) degrades the whole summary —
/// `child_total` cannot be invented and hiding that would lie about the
/// tree the summary describes.
fn summary_from(
    total: Result<u64, kb_core::Error>,
    session: &SessionOut,
    segment_chain: Vec<String>,
) -> SummaryLane {
    let Ok(total) = total else {
        return SummaryLane::unavailable("children unreadable");
    };
    SummaryLane {
        available: true,
        reason: None,
        title: session.title.clone(),
        display_name: session.display_name.clone(),
        harness: session.harness.clone(),
        project_slug: session.project_slug.clone(),
        substance: session.substance.clone(),
        active_secs: session.active_secs,
        started_at: session.started_at,
        ended_at: session.ended_at,
        segment_chain,
        root_session_id: session.root_session_id.clone(),
        parent_session_id: session.parent_session_id.clone(),
        child_total: total,
        resume: session.resume.clone(),
    }
}

async fn summary_lane(
    state: &Arc<KbHandles>,
    session: &SessionOut,
    session_id: &str,
) -> SummaryLane {
    let total = children_total(state, &children_predicate(session)).await;
    let chain_parts = session_chain_parts(state, session_id).await;
    let segment_chain = if chain_parts.len() >= 2 {
        chain_parts.iter().map(|p| p.session_id.clone()).collect()
    } else {
        Vec::new()
    };
    summary_from(total, session, segment_chain)
}

async fn children_lane(state: &Arc<KbHandles>, session: &SessionOut) -> ChildrenLane {
    let pred = children_predicate(session);
    let (total, page) = tokio::join!(
        children_total(state, &pred),
        children_page(state, &pred, None, CARD_CHILDREN_CAP)
    );
    // The total is the lane's headline number; when every kb's children
    // reader failed it cannot be invented — degrade rather than report 0
    // beside a page drained from the same dead readers.
    let Ok(total) = total else {
        return ChildrenLane::unavailable("children unreadable");
    };
    let (items, next_after) = page_overfetch_into(page, CARD_CHILDREN_CAP);
    ChildrenLane {
        available: true,
        reason: None,
        total,
        items: items.iter().map(child_row).collect(),
        next_after,
    }
}

async fn slate_lane(
    state: &Arc<KbHandles>,
    session: &SessionOut,
    peer: Option<SocketAddr>,
    headers: &HeaderMap,
    session_id: &str,
) -> SlateLane {
    let Some(raw_slug) = session.project_slug.clone() else {
        return SlateLane::unavailable("no project slug");
    };
    let slug = match SlateSlug::new(raw_slug) {
        Ok(s) => s,
        Err(_) => return SlateLane::unavailable("no project slug"),
    };
    let loopback = crate::middleware::is_loopback_origin(
        peer.map(|p| p.ip()),
        headers,
        &state.origin.trusted_proxies.load(),
    );
    match crate::routes::slates::load_for_read_async(state.paths.clone(), slug, loopback).await {
        Ok(Some(loaded)) => {
            let want_ref = format!("session:{session_id}");
            let mut posts: Vec<SlatePost> = loaded
                .posts
                .iter()
                .filter(|p| {
                    p.prov.session_id.as_deref() == Some(session_id) || p.refs.contains(&want_ref)
                })
                .map(|p| SlatePost {
                    seq: p.seq,
                    id: p.id.clone(),
                    at: p.at,
                    kind: p.kind.as_str().to_string(),
                    line: p.line.clone(),
                    topic: p.topic.clone(),
                })
                .collect();
            posts.sort_by_key(|p| std::cmp::Reverse(p.seq));
            let total = posts.len() as u32;
            let head_seq = loaded.meta.head_seq;
            let slug = loaded.meta.slug;
            SlateLane {
                available: true,
                reason: None,
                slug: Some(slug),
                head_seq: Some(head_seq),
                posts: posts.into_iter().take(CARD_SLATE_CAP).collect(),
                total,
            }
        }
        Ok(None) => SlateLane::unavailable("no slate"),
        Err(e) => {
            tracing::warn!(error = %e, "slate load failed in card");
            SlateLane::unavailable("slate unreadable")
        }
    }
}

fn live_lane(state: &Arc<KbHandles>, session_id: &str) -> LiveLane {
    let now_unix = chrono::Utc::now().timestamp();
    // `snapshot` is sync and drops its guard before returning (invariant
    // #15: no guard crosses an await); reduced to owned values here.
    let snap = state.live_registry.snapshot(now_unix);
    let Some((row, state2, confidence)) = snap
        .into_iter()
        .find(|(r, _, _)| r.session_id == session_id)
    else {
        return LiveLane::unavailable("not live");
    };
    // ONLY {state, holder, confidence, since_secs, blocked} — `last_tool`,
    // `background`, `last_line`, `cwd` need the live-status scrub grammar
    // and are deliberately absent from the card.
    LiveLane {
        available: true,
        reason: None,
        state: Some(state2),
        holder: Some(row.holder),
        confidence: Some(confidence),
        // Seconds since the registry's last activity, never negative.
        since_secs: Some((now_unix - row.last_activity_unix).max(0)),
        blocked: Some(row.blocked),
    }
}

/// T13 — the registry-derived terminal lane. One failing reader never
/// 500s the card (#28 drop-on-error); the registry read is infallible
/// in-memory state.
fn terminal_lane(
    state: &Arc<KbHandles>,
    peer: Option<std::net::IpAddr>,
    source: crate::middleware::IdentitySource,
    session_id: &str,
    root_session_id: Option<&str>,
) -> TerminalLane {
    let (enabled, refuse) = state
        .config
        .try_read()
        .map(|c| (c.terminal.enabled(), c.terminal.refuse_token_callers()))
        .unwrap_or((false, true));
    let admitted = crate::routes::identity::terminal_admitted_for(enabled, peer, source, refuse);
    if !enabled {
        return TerminalLane::unavailable("terminal disabled", admitted);
    }
    let now_unix = chrono::Utc::now().timestamp();
    let (term, via) = match state.live_registry.term_for_session(session_id, now_unix) {
        Some(t) => (Some(t), None),
        None => match root_session_id {
            Some(root) => match state.live_registry.term_for_session(root, now_unix) {
                Some(t) => (Some(t), Some("root".to_string())),
                None => (None, None),
            },
            None => (None, None),
        },
    };
    let Some(term) = term else {
        return TerminalLane::unavailable("not running in kb-term", admitted);
    };
    let Some((row, live_state, _)) = state.live_registry.session_for_term(&term, now_unix) else {
        return TerminalLane::unavailable("not running in kb-term", admitted);
    };
    TerminalLane {
        available: true,
        reason: None,
        term_id: Some(term),
        harness: Some(row.harness),
        state: Some(live_state),
        blocked: Some(row.blocked),
        admitted,
        via,
    }
}

fn map_check(c: crate::routes::sessions::SessionCheckOut) -> CardCheckOut {
    CardCheckOut {
        label: c.label,
        cmd: c.cmd,
        runners: c.runners,
        outcome: c.outcome,
        basis: c.basis,
        why: c.why,
        exit_code: c.exit_code,
        at: c.at,
        after_last_edit: c.after_last_edit,
        text: c.text,
    }
}

async fn outcome_lane(
    session: &SessionOut,
    files_rows: &[FileRow],
    commit_rows: &[CommitRow],
    files_ok: bool,
    commits_ok: bool,
    fanout_cap: usize,
) -> OutcomeLane {
    let verdict = session
        .evidence
        .as_ref()
        .map(|e| e.verdict)
        .unwrap_or(kb_core::sessions::checks::Verdict::NotExtracted);
    let checks = session
        .evidence_full
        .as_ref()
        .map(|ev| {
            ev.checks
                .iter()
                .map(|c| map_check(check_out(ev, c)))
                .collect()
        })
        .unwrap_or_default();
    let base = OutcomeLane {
        available: true,
        reason: None,
        last_assistant_text: session.outcome.clone(),
        verdict,
        checks,
        not_committed: None,
    };
    // Per-lane drop-on-error: `verdict`/`checks`/`last_assistant_text`
    // come from the already-loaded `SessionOut`, never from a failing
    // reader, so a files- or commits-lane failure must NOT cascade here.
    // Only `not_committed` depends on those readers; when either failed
    // it stays `None` (its Option models "not computed") and the failing
    // lane itself carries its own `unavailable`.
    if !files_ok || !commits_ok {
        return base;
    }
    OutcomeLane {
        not_committed: Some(compute_not_committed(files_rows, commit_rows, fanout_cap).await),
        ..base
    }
}

/// `not_committed` = the edited/wrote paths from the files lane minus the
/// union of the considered commits' touched files, over RESOLVED
/// `kind="commit"` rows only (at most the newest 20 by `seq`). Paths are
/// compared as absolute: a touched file's repo-relative path joins its
/// commit's own `repo_root`. The set difference and the partial-coverage
/// caption live in `kb_core::sessions::card`.
async fn compute_not_committed(
    files_rows: &[FileRow],
    commit_rows: &[CommitRow],
    fanout_cap: usize,
) -> NotCommitted {
    let edited: Vec<String> = files_rows
        .iter()
        .filter(|r| r.action == "edit" || r.action == "write")
        .map(|r| r.path.clone())
        .collect();
    let kind_commit: Vec<&CommitRow> = commit_rows.iter().filter(|r| r.kind == "commit").collect();
    let commits_total = kind_commit.len() as u32;
    let mut resolved: Vec<&CommitRow> =
        kind_commit.iter().filter(|r| r.resolved).copied().collect();
    resolved.reverse();
    let resolved_total = resolved.len() as u32;
    let capped = resolved_total > NOT_COMMITTED_COMMIT_CAP as u32;
    let considered: Vec<&CommitRow> = resolved
        .into_iter()
        .take(NOT_COMMITTED_COMMIT_CAP)
        .collect();
    // Every non-resolved kind=commit row is unusable coverage.
    let mut unresolved: u32 = commits_total - resolved_total;
    let mut touched: Vec<String> = Vec::new();
    let mut futs: Vec<super::CorpusFut<'_, Option<ResolvedTouched>>> = Vec::new();
    for row in &considered {
        let Some(repo_root) = row.repo_root.clone() else {
            // A resolved row without git resolution metadata is unusable
            // coverage too.
            unresolved += 1;
            continue;
        };
        futs.push(Box::pin(async move {
            touched_files_for_commit(row)
                .await
                .map(|(files, _)| (repo_root, files))
        }));
    }
    let results = super::buffered_join(futs, fanout_cap).await;
    for res in &results {
        match res {
            Some((root, files)) => {
                for f in files {
                    touched.push(
                        std::path::Path::new(root)
                            .join(&f.path)
                            .to_string_lossy()
                            .into_owned(),
                    );
                }
            }
            None => unresolved += 1,
        }
    }
    let files = not_committed_diff(&edited, &touched);
    let commits_considered = considered.len() as u32;
    let mut caption = not_committed_caption(commits_total, commits_considered, unresolved);
    // Git failures surface as empty touched lists: if every considered
    // commit answered with nothing and the diff is non-empty, the list
    // would otherwise read as confident.
    if !files.is_empty() && caption.is_none() && !considered.is_empty() && touched.is_empty() {
        caption = Some(
            "touched-files lookup returned nothing for the considered commits; files may be committed earlier"
                .to_string(),
        );
    }
    NotCommitted {
        files,
        commits_considered,
        commits_total,
        capped,
        unresolved_commits: unresolved,
        caption,
    }
}

// ---------------------------------------------------------------------------
// Handlers
// ---------------------------------------------------------------------------

/// `GET /api/sessions/{session_id}/card` — one daemon-rendered
/// `kb-session-card/1` for the newest capture of a session. Lanes run
/// concurrently as their own futures; each degrades independently
/// (drop-on-error). 404 when no kb holds the session row.
pub async fn card(
    State(state): State<Arc<KbHandles>>,
    Extension(identity): Extension<Identity>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Path(session_id): Path<String>,
) -> Response {
    let Some(session) = find_session(&state, &session_id).await else {
        return error_to_problem_json(&kb_core::Error::NotFound(format!("session {session_id}")));
    };
    let session_id = session.session_id.clone();
    // The widened memory read-id set (`get`'s rule).
    let read_ids_parts = session_chain_parts(&state, &session_id).await;
    let read_ids = widen_session_ids(&session_id, &read_ids_parts);
    let peer_ip = peer.ip();

    let (
        summary,
        (files, files_rows, files_ok),
        decisions,
        (commits, commit_rows, commits_ok),
        research,
        comments,
        readings,
        memory,
        live,
        slate,
        children,
    ) = tokio::join!(
        summary_lane(&state, &session, &session_id),
        files_lane(&state, &session, &session_id),
        decisions_lane(&state, &session, &session_id),
        commits_lane(&state, &session, &session_id),
        research_lane(&state, &session_id),
        comments_lane(&state, &session),
        readings_lane(&state, &session),
        memory_lane(&state, &session, &read_ids),
        async { live_lane(&state, &session_id) },
        slate_lane(&state, &session, Some(peer), &headers, &session_id),
        children_lane(&state, &session)
    );

    let outcome = outcome_lane(
        &session,
        &files_rows,
        &commit_rows,
        files_ok,
        commits_ok,
        state.fanout_cap,
    )
    .await;

    // Compute the terminal lane BEFORE moving `summary` into the card:
    // the child case resolves through the summary's root_session_id.
    let root_sid = summary.root_session_id.clone();
    let terminal = terminal_lane(
        &state,
        Some(peer_ip),
        identity.source,
        &session_id,
        root_sid.as_deref(),
    );

    let mut card = SessionCard {
        schema: SESSION_CARD_SCHEMA.to_string(),
        session_id: session_id.clone(),
        kb: session.kb.clone(),
        artifact_id: session.artifact_id.clone(),
        text: String::new(),
        summary,
        outcome,
        files,
        decisions,
        commits,
        research,
        comments,
        readings,
        memory,
        live,
        slate,
        children,
        terminal,
    };
    card.text = render_text(&card);
    Json(card).into_response()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn after_cursor_round_trips_and_malformed_400s() {
        let mut out = SessionOut::from_row(
            "smoke",
            SqliteRow {
                artifact_id: "a1".into(),
                session_id: "sess:with:colons".into(),
                started_at: 1_700_000_000,
                ..Default::default()
            },
        );
        out.display_name = "x".into();
        let enc = encode_after(&out);
        assert_eq!(enc, "1700000000:sess:with:colons");
        let (at, sid) = parse_after(&enc).unwrap();
        assert_eq!((at, sid), (1_700_000_000, "sess:with:colons"));
        assert!(parse_after("nope").is_err());
        assert!(parse_after("12:").is_err());
        assert!(parse_after("x:sid").is_err());
    }

    #[test]
    fn children_total_matches_card_children_total() {
        // The predicate helper is the one place the two agree: a root's
        // flood is its children whether the card or /children asks, and a
        // non-root session's children are its direct descendants.
        let mut root = SessionOut::from_row(
            "smoke",
            SqliteRow {
                artifact_id: "a1".into(),
                session_id: "root-1".into(),
                ..Default::default()
            },
        );
        assert!(matches!(
            children_predicate(&root),
            ChildrenPredicate::Root(_)
        ));
        root.root_session_id = Some("root-1".into());
        assert!(matches!(
            children_predicate(&root),
            ChildrenPredicate::Parent(_)
        ));
        let (rt, rp) = children_predicate(&root).args();
        assert_eq!(rt, None);
        assert_eq!(rp.as_deref(), Some("root-1"));
    }

    #[test]
    fn page_overfetch_splits_and_marks_next_cursor() {
        let mk = |at: i64, id: &str| {
            SessionOut::from_row(
                "smoke",
                SqliteRow {
                    artifact_id: "a".into(),
                    session_id: id.into(),
                    started_at: at,
                    ..Default::default()
                },
            )
        };
        let page = vec![mk(1, "c1"), mk(2, "c2"), mk(3, "c3")];
        let (items, next) = page_overfetch_into(page, 2);
        assert_eq!(items.len(), 2);
        assert_eq!(next.as_deref(), Some("2:c2"));
        let page = vec![mk(1, "c1")];
        let (items, next) = page_overfetch_into(page, 50);
        assert_eq!(items.len(), 1);
        assert_eq!(next, None);
    }

    #[test]
    fn reader_family_dark_only_when_every_kb_failed() {
        // Zero kbs: no reader to fail, never dark.
        assert!(!reader_family_dark(0, 0));
        // One kb, failed: dark. One kb, answered: not dark.
        assert!(reader_family_dark(1, 1));
        assert!(!reader_family_dark(0, 1));
        // Two kbs, one answered: partial is honest, not dark.
        assert!(!reader_family_dark(1, 2));
        // Two kbs, both failed: dark.
        assert!(reader_family_dark(2, 2));
    }

    #[test]
    fn summary_marks_unavailable_when_children_unreadable() {
        let out = SessionOut::from_row(
            "smoke",
            SqliteRow {
                artifact_id: "a1".into(),
                session_id: "root-1".into(),
                title: Some("synthetic title".into()),
                ..Default::default()
            },
        );
        let lane = summary_from(Err(kb_core::Error::Storage("x".into())), &out, Vec::new());
        assert_eq!(lane.available, false);
        assert_eq!(lane.reason.as_deref(), Some("children unreadable"));
        let lane = summary_from(Ok(3), &out, Vec::new());
        assert_eq!(lane.available, true);
        assert_eq!(lane.child_total, 3);
        assert_eq!(lane.title.as_deref(), Some("synthetic title"));
    }

    #[tokio::test]
    async fn children_total_from_unions_ids_and_degrades_when_all_kbs_fail() {
        use kb_core::storage::actor::StorageActor;
        // Two sources: a healthy one and one whose `sessions` table is
        // dropped out from under its actor (a real storage failure).
        let tmp_ok = tempfile::tempdir().unwrap();
        let ok_sqlite = tmp_ok.path().join("ok.db");
        let ok_handle = StorageActor::spawn(tmp_ok.path().join("lance"), ok_sqlite.clone(), None)
            .await
            .expect("spawn healthy storage actor");
        {
            let mut db = kb_core::storage::sqlite::Db::open(&ok_sqlite).expect("open healthy db");
            let mut root = SqliteRow {
                artifact_id: "a-root".into(),
                session_id: "ut-root".into(),
                started_at: 1_000,
                ..Default::default()
            };
            db.sessions_upsert(&root).unwrap();
            let mut c = SqliteRow {
                artifact_id: "a-c".into(),
                session_id: "ut-child".into(),
                started_at: 1_100,
                ..Default::default()
            };
            c.root_session_id = Some("ut-root".into());
            c.parent_session_id = Some("ut-root".into());
            db.sessions_upsert(&c).unwrap();
            root.session_id = "ut-root".into();
        }
        let tmp_bad = tempfile::tempdir().unwrap();
        let bad_sqlite = tmp_bad.path().join("bad.db");
        let bad_handle =
            StorageActor::spawn(tmp_bad.path().join("lance"), bad_sqlite.clone(), None)
                .await
                .expect("spawn failing storage actor");
        {
            // A real storage failure: drop the `sessions` table the actor's
            // connection reads through. Every later query misses with
            // "no such table: sessions".
            let raw = rusqlite::Connection::open(&bad_sqlite).expect("raw open");
            raw.execute_batch("DROP TABLE sessions")
                .expect("drop sessions");
        }
        // Healthy alone: the union counts the one child.
        let pred_root = ChildrenPredicate::Root("ut-root".to_string());
        let n = children_total_from(&[("smoke".to_string(), ok_handle.clone())], 8, &pred_root)
            .await
            .expect("healthy total");
        assert_eq!(n, 1);
        // Healthy + dropped-table: partial success keeps the count.
        let n = children_total_from(
            &[
                ("smoke".to_string(), ok_handle.clone()),
                ("broken".to_string(), bad_handle.clone()),
            ],
            8,
            &pred_root,
        )
        .await
        .expect("partial failure keeps the healthy count");
        assert_eq!(n, 1);
        // Dropped-table alone: every source failed, so the total is an
        // Err — the summary/children lanes degrade instead of reporting 0.
        let err = children_total_from(&[("broken".to_string(), bad_handle)], 8, &pred_root).await;
        assert!(err.is_err(), "all-failed total must be Err, got {err:?}");
    }
}
