//! A5 — `GET /api/session-card?sid=<sid>[&repo=<name>]` (`kbc-session-card/1`):
//! one cross-origin read answering "what did this session do on the code
//! side" for kb's SPA (the per-session page at `/sessions/:sid`).
//!
//! Four lanes, each `{available, caption?, ...}`; a failing lane never fails
//! the request (its `available:false` carries a closed-vocabulary caption,
//! never a 5xx):
//!
//! - **commits** — kb's `session_commits` feed, kept to `kind == "commit"` and
//!   resolved through the join ladder, unioned with the ladder's own
//!   `commit_sessions` cache rows for the same session. A commit whose repo
//!   is not mirrored here is listed as `visible:false` with caption
//!   "repo not configured here"; a mirrored commit that does not resolve in
//!   this checkout is `visible:false` with caption "commit not in this
//!   checkout" (the ladder's review-store fallback is deliberately NOT
//!   consulted for visibility — see `join::ladder::resolve_commit`).
//! - **reviews** — local review rows the session started (`reviews.session_id`)
//!   unioned with reviews whose document revision names the session as its
//!   author (`review_docs.author_json.session_id`), tagged by `basis`.
//! - **findings** — unsuperseded `review_findings` authored by this session,
//!   grouped by severity, `basis: "author"`. The author is the shared
//!   `"session:<sid>"` prefix (`behavioral::SESSION_AUTHOR_PREFIX`); it
//!   matches either `review_findings.author` or the linked
//!   `annotations.author`. `review_findings.author` is usually NULL for
//!   imported findings, so this count is often 0 — the honest answer to
//!   "findings this session wrote".
//! - **presence** — kb's `live_touches` relay, reduced to a DISTINCT FILE
//!   COUNT and the newest age for this one session. No paths, no file
//!   contents, no transcript text, no prompt text, no repo-relative paths
//!   ever appear in this body — presence is a count plus an age. The same
//!   holds for every other lane: this route is on the credentialed doc-lens
//!   CORS set (`router.rs`'s `doclens_read`) because kb's SPA fetches it
//!   cross-origin, and it carries strictly less than the doc lens already
//!   served to that origin.
//!
//! The sid is validated BEFORE any kb call: `KbClient::session_commits`
//! interpolates it into a URL path unescaped, so it must be a plain token
//! (the same rule `kb_core`'s resume table uses — duplicated here because
//! that rule is private there; the sources are grep-pinned to each other by
//! the unit test below, which is the review contract for the duplication).
//! A rejected sid is never echoed in the error body.

use std::collections::HashSet;
use std::sync::Arc;

use axum::extract::{Query, State};
use axum::http::header;
use axum::response::IntoResponse;
use axum::Json;
use futures::stream::{self, StreamExt};
use serde::{Deserialize, Serialize};

use crate::behavioral;
use crate::config::RepoEntry;
use crate::join::kb_client::{KbClientError, LiveTouch, SessionCommitEntry};
use crate::join::ladder::{resolve_commit, Attribution, Confidence};
use crate::presence;
use crate::routes::ApiError;
use crate::state::SharedState;
use crate::store::{CommitSessionRow, SessionReviewRow, SeverityCounts, StoreBlocking};

/// The wire schema (`kbc-session-card/1`).
pub const SCHEMA: &str = "kbc-session-card/1";
/// Max commits one card shows (ladder runs on at most these; `total` stays
/// the full count and `truncated` says the cap was hit).
pub const MAX_COMMITS: usize = 20;
/// Max reviews one card shows.
pub const MAX_REVIEWS: usize = 20;
/// Ladder concurrency for the commits lane.
pub const LADDER_CONCURRENCY: usize = 4;

/// Caption for a local store failure (closes the presence `CAP_*`
/// vocabulary: every lane failure is one of those captions or this one).
const CAP_STORE_ERROR: &str = "store error";

#[derive(Debug, Deserialize)]
pub struct SessionCardParams {
    pub sid: Option<String>,
    pub repo: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SessionCardOut {
    pub schema: &'static str,
    pub sid: String,
    pub now_unix: i64,
    pub commits: CommitsLane,
    pub reviews: ReviewsLane,
    pub findings: FindingsLane,
    pub presence: PresenceLane,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct CommitsLane {
    pub available: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub caption: Option<String>,
    pub total: usize,
    pub shown: usize,
    pub truncated: bool,
    pub items: Vec<CommitItem>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct CommitItem {
    pub repo: String,
    pub sha: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub subject: Option<String>,
    pub visible: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub caption: Option<String>,
    pub confidence: Confidence,
    pub via: String,
    pub attributed_here: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ReviewsLane {
    pub available: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub caption: Option<String>,
    pub total: usize,
    pub shown: usize,
    pub items: Vec<ReviewItem>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ReviewItem {
    pub id: i64,
    pub repo: String,
    pub title: String,
    pub state: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub verdict: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub verdict_ps: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pr_number: Option<i64>,
    pub updated_at: i64,
    pub basis: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct FindingsLane {
    pub available: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub caption: Option<String>,
    pub total: usize,
    pub by_severity: SeverityCounts,
    pub basis: &'static str,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct PresenceLane {
    pub available: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub caption: Option<String>,
    pub files: usize,
    pub newest_age_secs: i64,
}

// --- sid validation ----------------------------------------------------

/// A plain token: non-empty, at most 128 bytes, starts alphanumeric, and
/// only alphanumerics or `.` `_` `:` `-` after that. DUPLICATED from
/// `kb_core::sessions::resume::is_plain_token` (private there) — see the
/// module doc; the two must stay identical.
pub fn is_plain_session_token(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 128
        && s.as_bytes()[0].is_ascii_alphanumeric()
        && s.as_bytes()
            .iter()
            .all(|&c| c.is_ascii_alphanumeric() || matches!(c, b'.' | b'_' | b':' | b'-'))
}

// --- commits: planning (pure) -----------------------------------------

/// The join ladder's own repo-alignment rule
/// (`sessiondiff::repo_root_matches`, private there): canonical path
/// equality, trailing slash tolerated. Duplicated with a comment rather
/// than imported, same precedent as `sessiondiff`'s own duplication.
fn repo_root_matches(repo: &RepoEntry, repo_root: &str) -> bool {
    repo.path.to_string_lossy().trim_end_matches('/') == repo_root.trim_end_matches('/')
}

/// One kb commit, ready for resolution. `repo: None` means unmirrored
/// (no configured repo owns `repo_root`).
pub(crate) struct CommitPlan {
    pub sha: String,
    pub subject: Option<String>,
    pub repo: Option<usize>,
}

/// Keep `kind == "commit"` only (as `sessiondiff` does), drop sha-less
/// entries, map each survivor to its configured repo (or unmirrored), and
/// honour the `?repo=` filter the same way `sessiondiff` does (a resolved
/// repo other than the filter is excluded; an unmirrored entry is excluded
/// when a filter is set).
pub(crate) fn plan_commits(
    entries: &[SessionCommitEntry],
    repos: &[RepoEntry],
    filter: Option<usize>,
) -> Vec<CommitPlan> {
    let mut out = Vec::new();
    for e in entries.iter().filter(|e| e.kind == "commit") {
        let Some(sha) = e.sha_full.clone().or_else(|| e.sha.clone()) else {
            continue;
        };
        let repo = e
            .repo_root
            .as_deref()
            .and_then(|root| repos.iter().position(|r| repo_root_matches(r, root)));
        match (repo, filter) {
            (Some(r), Some(f)) if r == f => out.push(CommitPlan {
                sha,
                subject: e.subject.clone(),
                repo: Some(r),
            }),
            (Some(_), Some(_)) => {}
            (Some(r), None) => out.push(CommitPlan {
                sha,
                subject: e.subject.clone(),
                repo: Some(r),
            }),
            (None, Some(_)) => {}
            (None, None) => out.push(CommitPlan {
                sha,
                subject: e.subject.clone(),
                repo: None,
            }),
        }
    }
    out
}

/// The wire item for a ladder-resolved commit. `attributed_here` is the
/// ladder's own session verdict compared against the requested sid —
/// never hidden, never guessed.
pub(crate) fn commit_item_from_attribution(
    sid: &str,
    repo: String,
    sha: String,
    subject: Option<String>,
    attribution: &Attribution,
) -> CommitItem {
    CommitItem {
        repo,
        sha,
        subject,
        visible: true,
        caption: None,
        confidence: attribution.confidence,
        via: attribution.via.clone(),
        attributed_here: attribution.session_id.as_deref() == Some(sid),
    }
}

/// The honest item for a commit this daemon cannot see: no repo owns it
/// (`via: "unmirrored"`), or the owning checkout does not have it
/// (`via: "not-in-checkout"`). `repo` is empty for unmirrored — the kb
/// entry carries only a root path, and paths never enter this body.
pub(crate) fn commit_item_not_visible(
    repo: String,
    sha: String,
    subject: Option<String>,
    caption: &str,
    via: &str,
) -> CommitItem {
    CommitItem {
        repo,
        sha,
        subject,
        visible: false,
        caption: Some(caption.to_string()),
        confidence: Confidence::None,
        via: via.to_string(),
        attributed_here: false,
    }
}

/// A cache-only extra: already resolved by some earlier ladder run, so
/// its cached confidence/via are carried verbatim.
pub(crate) fn commit_item_from_cache(
    sid: &str,
    repo: String,
    sha: String,
    row: &CommitSessionRow,
) -> CommitItem {
    CommitItem {
        repo,
        sha,
        subject: None,
        visible: true,
        caption: None,
        confidence: Confidence::parse(&row.confidence).unwrap_or(Confidence::None),
        via: row.via.clone(),
        attributed_here: row.session_id.as_deref() == Some(sid),
    }
}

/// Cap the merged list at `MAX_COMMITS`, keeping `total` the full count.
pub(crate) fn cap_commits(items: Vec<CommitItem>, caption: Option<String>) -> CommitsLane {
    let total = items.len();
    let truncated = total > MAX_COMMITS;
    let shown = total.min(MAX_COMMITS);
    CommitsLane {
        available: true,
        caption,
        total,
        shown,
        truncated,
        items: items.into_iter().take(MAX_COMMITS).collect(),
    }
}

pub(crate) fn commits_unavailable(caption: &str) -> CommitsLane {
    CommitsLane {
        available: false,
        caption: Some(caption.to_string()),
        total: 0,
        shown: 0,
        truncated: false,
        items: Vec::new(),
    }
}

// --- reviews (pure) -----------------------------------------------------

pub(crate) fn review_item_from_row(row: &SessionReviewRow, basis: &str) -> ReviewItem {
    ReviewItem {
        id: row.id,
        repo: row.repo.clone(),
        title: row.title.clone().unwrap_or_default(),
        state: row.state.clone(),
        verdict: row.verdict.clone(),
        verdict_ps: row.verdict_ps,
        pr_number: row.pr_number,
        updated_at: row.updated_at,
        basis: basis.to_string(),
    }
}

/// Sort newest-first (`updated_at DESC, id DESC`), cap at `MAX_REVIEWS`.
pub(crate) fn reviews_lane(mut items: Vec<ReviewItem>) -> ReviewsLane {
    items.sort_by(|a, b| (b.updated_at, b.id).cmp(&(a.updated_at, a.id)));
    let total = items.len();
    let shown = total.min(MAX_REVIEWS);
    items.truncate(MAX_REVIEWS);
    ReviewsLane {
        available: true,
        caption: None,
        total,
        shown,
        items,
    }
}

pub(crate) fn reviews_unavailable(caption: &str) -> ReviewsLane {
    ReviewsLane {
        available: false,
        caption: Some(caption.to_string()),
        total: 0,
        shown: 0,
        items: Vec::new(),
    }
}

// --- findings (pure) -----------------------------------------------------

pub(crate) fn findings_lane(counts: &SeverityCounts) -> FindingsLane {
    FindingsLane {
        available: true,
        caption: None,
        total: counts.blocker + counts.concern + counts.ok,
        by_severity: *counts,
        basis: "author",
    }
}

pub(crate) fn findings_unavailable(caption: &str) -> FindingsLane {
    FindingsLane {
        available: false,
        caption: Some(caption.to_string()),
        total: 0,
        by_severity: SeverityCounts::default(),
        basis: "author",
    }
}

// --- presence (pure) -----------------------------------------------------

/// Worktree-basename sanity, duplicated from `presence`'s own row filter
/// (private there): a label we cannot show must not turn into "the main
/// checkout" — such a touch is skipped, never guessed.
fn worktree_ok(w: &str) -> bool {
    !w.is_empty()
        && w.len() <= 128
        && !w.contains('/')
        && !w.contains('\\')
        && !w.chars().any(char::is_control)
}

/// Reduce kb's live touches to a count + newest age for one session.
/// Mirrors `presence`'s mapping (alias > name > path basename, ambiguous /
/// unmatched skipped) and its row sanity (`rel_ok`, worktree), then counts
/// distinct `(repo, worktree, rel)` pairs. A truncated feed names
/// `CAP_TRUNCATED`.
pub(crate) fn presence_lane(
    touches: &[LiveTouch],
    repos: &[RepoEntry],
    sid: &str,
    truncated: bool,
) -> PresenceLane {
    let mut seen: HashSet<(usize, Option<String>, String)> = HashSet::new();
    let mut newest_age_secs: Option<i64> = None;
    for t in touches.iter().filter(|t| t.session_id == sid) {
        let presence::Mapped::Repo(i) = presence::map_project(repos, &t.project) else {
            continue;
        };
        if !presence::rel_ok(&t.rel) {
            continue;
        }
        let worktree = match t.worktree.as_deref() {
            None => None,
            Some(w) if worktree_ok(w) => Some(w.to_string()),
            Some(_) => continue,
        };
        seen.insert((i, worktree, t.rel.clone()));
        newest_age_secs = Some(match newest_age_secs {
            None => t.age_secs.max(0),
            Some(cur) => cur.min(t.age_secs.max(0)),
        });
    }
    PresenceLane {
        available: true,
        caption: truncated.then(|| presence::CAP_TRUNCATED.to_string()),
        files: seen.len(),
        newest_age_secs: newest_age_secs.unwrap_or(0),
    }
}

pub(crate) fn presence_unavailable(caption: &str) -> PresenceLane {
    PresenceLane {
        available: false,
        caption: Some(caption.to_string()),
        files: 0,
        newest_age_secs: 0,
    }
}

// --- the route ------------------------------------------------------------

fn join_captions(caps: &[&str]) -> Option<String> {
    let caps: Vec<&str> = caps.iter().copied().filter(|c| !c.is_empty()).collect();
    (!caps.is_empty()).then(|| caps.join("; "))
}

/// `GET /api/session-card?sid=<sid>[&repo=<name>]`.
pub async fn session_card_route(
    State(state): State<SharedState>,
    Query(p): Query<SessionCardParams>,
) -> Result<impl IntoResponse, ApiError> {
    let sid = p.sid.as_deref().unwrap_or("").trim();
    if !is_plain_session_token(sid) {
        // Never echo a rejected sid (untrusted input, no consumer needs it).
        return Err(ApiError::bad_request(
            "sid is required and must be a plain token",
        ));
    }
    let now_unix = chrono::Utc::now().timestamp();
    let repo_filter: Option<usize> =
        match p.repo.as_deref().map(str::trim).filter(|r| !r.is_empty()) {
            Some(name) => Some(
                state
                    .repos
                    .iter()
                    .position(|r| r.name == name)
                    .ok_or_else(|| ApiError::bad_request("unknown repo"))?,
            ),
            None => None,
        };

    // Commits and presence run concurrently; both are kb reads through the
    // one client. Store readers go through `run_blocking` (the store
    // starvation rule); no `std::sync::Mutex` guard crosses an await.
    let (kb_commits, kb_touches) = tokio::join!(
        state.kb_client.session_commits(sid),
        state.kb_client.live_touches(&[], presence::MAX_AGE_SECS),
    );
    let (commits, reviews, findings) = tokio::join!(
        commits_lane_fetch(&state, sid, repo_filter, &kb_commits),
        reviews_lane_fetch(&state, sid, repo_filter),
        findings_lane_fetch(&state, sid),
    );
    let presence = match &kb_touches {
        Ok(relay) => presence_lane(&relay.touches, &state.repos, sid, relay.truncated),
        Err(e) => presence_unavailable(presence::failure_caption(e)),
    };

    let card = SessionCardOut {
        schema: SCHEMA,
        sid: sid.to_string(),
        now_unix,
        commits,
        reviews,
        findings,
        presence,
    };
    Ok(([(header::CACHE_CONTROL, "no-store")], Json(card)))
}

/// The commits lane: kb feed order first (ladder-resolved), then
/// cache-only extras sorted by `(repo, sha)`, deduped by `(repo, sha)` with
/// the kb side winning. The ladder runs only on the first `MAX_COMMITS`
/// kb items. When kb fails but the cache has rows the lane is still
/// available (rows from cache, kb's failure caption).
async fn commits_lane_fetch(
    state: &Arc<crate::state::AppState>,
    sid: &str,
    repo_filter: Option<usize>,
    kb_result: &Result<Vec<SessionCommitEntry>, KbClientError>,
) -> CommitsLane {
    let cache_result = state
        .store
        .run_blocking({
            let sid = sid.to_string();
            move |s| s.commit_sessions_for_session(&sid, MAX_COMMITS)
        })
        .await;
    let cache_failed = cache_result.is_err();

    let mut captions: Vec<String> = Vec::new();
    let mut items: Vec<CommitItem> = Vec::new();

    match kb_result {
        Ok(entries) => {
            let plans = plan_commits(entries, &state.repos, repo_filter);
            // First pass: visibility. Unmirrored and not-in-checkout items
            // are final; mirrored+resolved ones queue for the ladder.
            let mut next: Vec<Option<CommitItem>> = Vec::with_capacity(plans.len());
            let mut ladder_jobs: Vec<(usize, usize, String)> = Vec::new();
            for (i, plan) in plans.iter().enumerate() {
                match plan.repo {
                    None => next.push(Some(commit_item_not_visible(
                        String::new(),
                        plan.sha.clone(),
                        plan.subject.clone(),
                        "repo not configured here",
                        "unmirrored",
                    ))),
                    Some(r) => {
                        if i >= MAX_COMMITS {
                            // Past the cap the lane never shows this item;
                            // skip the ladder run entirely (it is dropped by
                            // `cap_commits`).
                            next.push(Some(commit_item_not_visible(
                                state.repos[r].name.clone(),
                                plan.sha.clone(),
                                plan.subject.clone(),
                                "commit not in this checkout",
                                "not-in-checkout",
                            )));
                            continue;
                        }
                        let local =
                            crate::join::local::resolve_local(&state.repos[r].path, &plan.sha)
                                .await;
                        if !local.resolved {
                            // Deliberately ignores the review-store fallback
                            // inside `resolve_commit` (join/ladder.rs):
                            // visibility is "is it in THIS checkout", not
                            // "can the review store resolve it".
                            next.push(Some(commit_item_not_visible(
                                state.repos[r].name.clone(),
                                plan.sha.clone(),
                                plan.subject.clone(),
                                "commit not in this checkout",
                                "not-in-checkout",
                            )));
                        } else {
                            ladder_jobs.push((i, r, plan.sha.clone()));
                            next.push(None);
                        }
                    }
                }
            }
            if !ladder_jobs.is_empty() {
                let resolved: Vec<(usize, Attribution)> = stream::iter(ladder_jobs)
                    .map(|(i, repo_idx, sha)| {
                        let state = state.clone();
                        async move {
                            // Every configured repo has a store id (boot
                            // registers them; `routes::find_repo` upholds the
                            // same invariant) — a miss means the cache write
                            // inside `resolve_commit` fails FK and is logged,
                            // never fatal.
                            let repo_id = state
                                .repo_ids
                                .get(&state.repos[repo_idx].name)
                                .copied()
                                .unwrap_or(0);
                            let attribution = resolve_commit(
                                &state.repos[repo_idx],
                                repo_id,
                                &sha,
                                &state.store,
                                &state.kb_client,
                            )
                            .await;
                            (i, attribution)
                        }
                    })
                    .buffer_unordered(LADDER_CONCURRENCY)
                    .collect()
                    .await;
                for (i, attribution) in resolved {
                    let plan = &plans[i];
                    let repo_idx = plan.repo.unwrap_or(0);
                    next[i] = Some(commit_item_from_attribution(
                        sid,
                        state.repos[repo_idx].name.clone(),
                        plan.sha.clone(),
                        plan.subject.clone(),
                        &attribution,
                    ));
                }
            }
            items.extend(next.into_iter().flatten());
        }
        Err(e) => captions.push(presence::failure_caption(e).to_string()),
    }

    match cache_result {
        Ok(cache) => {
            let seen: HashSet<(String, String)> = items
                .iter()
                .map(|it| (it.repo.clone(), it.sha.clone()))
                .collect();
            let mut extras: Vec<(String, String, CommitSessionRow)> = cache
                .into_iter()
                .filter(|(repo, sha, _)| {
                    !seen.contains(&(repo.clone(), sha.clone()))
                        && repo_filter.is_none_or(|f| state.repos[f].name == *repo)
                })
                .collect();
            extras.sort_by(|a, b| (a.0.clone(), a.1.clone()).cmp(&(b.0.clone(), b.1.clone())));
            items.extend(
                extras
                    .into_iter()
                    .map(|(repo, sha, row)| commit_item_from_cache(sid, repo, sha, &row)),
            );
        }
        Err(_) => captions.push(CAP_STORE_ERROR.to_string()),
    }

    if items.is_empty() && (kb_result.is_err() || cache_failed) {
        let caption = captions
            .first()
            .map(String::as_str)
            .unwrap_or(presence::CAP_UNREACHABLE);
        return commits_unavailable(caption);
    }
    cap_commits(
        items,
        join_captions(&captions.iter().map(String::as_str).collect::<Vec<_>>()),
    )
}

/// The reviews lane: `session_id` rows (basis `"session_id"`) unioned with
/// reviews whose document revision names this session as author (basis
/// `"doc_author"`; the session_id half wins a duplicate).
async fn reviews_lane_fetch(
    state: &Arc<crate::state::AppState>,
    sid: &str,
    repo_filter: Option<usize>,
) -> ReviewsLane {
    let repo_name = repo_filter.map(|i| state.repos[i].name.clone());
    let sid = sid.to_string();
    let fetched = state
        .store
        .run_blocking(move |s| {
            let a = s.reviews_for_session(&sid, repo_name.as_deref(), MAX_REVIEWS);
            let doc_ids = s.review_ids_authored_by_session(&sid);
            match (a, doc_ids) {
                (Ok(a), Ok(doc_ids)) => {
                    let missing: Vec<i64> = doc_ids
                        .into_iter()
                        .filter(|id| !a.iter().any(|r| r.id == *id))
                        .collect();
                    if missing.is_empty() {
                        Ok((a, Vec::new()))
                    } else {
                        match (
                            s.get_reviews_by_ids(&missing),
                            s.get_review_pr_bindings(&missing),
                        ) {
                            (Ok(by_id), Ok(bindings)) => Ok((
                                a,
                                missing
                                    .into_iter()
                                    .filter_map(|id| {
                                        by_id.get(&id).map(|r| {
                                            (r.clone(), bindings.get(&id).and_then(|b| b.pr_number))
                                        })
                                    })
                                    .collect(),
                            )),
                            (Err(e), _) => Err(e),
                            (_, Err(e)) => Err(e),
                        }
                    }
                }
                (Err(e), _) => Err(e),
                (_, Err(e)) => Err(e),
            }
        })
        .await;
    match fetched {
        Err(_) => reviews_unavailable(CAP_STORE_ERROR),
        Ok((a_rows, doc_rows)) => {
            let mut items: Vec<ReviewItem> = a_rows
                .iter()
                .map(|r| review_item_from_row(r, "session_id"))
                .collect();
            for (row, pr_number) in &doc_rows {
                if let Some(f) = repo_filter {
                    if state.repos[f].name != row.repo {
                        continue;
                    }
                }
                items.push(ReviewItem {
                    id: row.id,
                    repo: row.repo.clone(),
                    title: row.title.clone().unwrap_or_default(),
                    state: row.state.clone(),
                    verdict: row.verdict.clone(),
                    verdict_ps: row.verdict_ps,
                    pr_number: *pr_number,
                    updated_at: row.updated_at,
                    basis: "doc_author".to_string(),
                });
            }
            reviews_lane(items)
        }
    }
}

/// The findings lane: unsuperseded findings this session wrote, counted by
/// severity under the shared `"session:<sid>"` author prefix (never
/// hardcoded).
async fn findings_lane_fetch(state: &Arc<crate::state::AppState>, sid: &str) -> FindingsLane {
    let author = format!("{}{}", behavioral::SESSION_AUTHOR_PREFIX, sid);
    match state
        .store
        .run_blocking(move |s| s.finding_severity_counts_authored_by(&author))
        .await
    {
        Ok(counts) => findings_lane(&counts),
        Err(_) => findings_unavailable(CAP_STORE_ERROR),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::annotations::anchor_for_line;
    use crate::join::kb_client::SessionCommitEntry;
    use crate::store::{
        NewReviewFinding, Store, FINDING_ORIGIN_MANUAL, LOCATION_KIND_SINGLE, SEVERITY_BLOCKER,
        SEVERITY_CONCERN, SEVERITY_OK,
    };
    use std::path::PathBuf;

    const SID: &str = "sess-0001";
    const SHA_A: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
    const SHA_B: &str = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";
    const SHA_C: &str = "cccccccccccccccccccccccccccccccccccccccc";

    fn repo_entry(name: &str) -> RepoEntry {
        RepoEntry {
            name: name.to_string(),
            path: PathBuf::from(format!("/repos/{name}")),
            projects: Vec::new(),
        }
    }

    fn entry(sha: &str, repo_root: Option<&str>) -> SessionCommitEntry {
        SessionCommitEntry {
            kind: "commit".to_string(),
            sha: Some(sha.to_string()),
            subject: Some("fixture subject".to_string()),
            resolved: true,
            sha_full: Some(sha.to_string()),
            repo_root: repo_root.map(str::to_string),
            author: None,
            parents: None,
            trailers: Vec::new(),
        }
    }

    fn open_temp() -> (tempfile::TempDir, Store) {
        let tmp = tempfile::tempdir().unwrap();
        let store = Store::open(&tmp.path().join("index.db")).unwrap();
        (tmp, store)
    }

    #[test]
    fn sid_validation_matches_the_resume_token_rule() {
        for ok in ["abc-123", "x:y.z_1", &"a".repeat(128), "s-p01"] {
            assert!(is_plain_session_token(ok), "{ok:?} must be accepted");
        }
        for bad in [
            "",
            &"a".repeat(129),
            "-abc",
            "a/b",
            "a b",
            "..",
            "a\n",
            "héllo",
        ] {
            assert!(!is_plain_session_token(bad), "{bad:?} must be rejected");
        }
        // The brief's non-ASCII case, spelled out.
        assert!(!is_plain_session_token("séss-0001"));
    }

    #[test]
    fn commits_lane_marks_unmirrored_and_unknown_repo_honestly() {
        let repos = vec![repo_entry("fixture")];
        let entries = vec![
            entry(SHA_A, None),
            entry(SHA_B, Some("/repos/unknown-elsewhere")),
            entry(SHA_C, Some("/repos/fixture")),
        ];
        let plans = plan_commits(&entries, &repos, None);
        assert_eq!(plans.len(), 3);
        // No repo_root at all -> unmirrored.
        assert!(plans[0].repo.is_none());
        // A root no configured repo owns -> unmirrored (same honest item).
        assert!(plans[1].repo.is_none());
        assert_eq!(plans[2].repo, Some(0));

        let lane = cap_commits(
            plans
                .iter()
                .map(|p| match p.repo {
                    None => commit_item_not_visible(
                        String::new(),
                        p.sha.clone(),
                        p.subject.clone(),
                        "repo not configured here",
                        "unmirrored",
                    ),
                    Some(0) => CommitItem {
                        repo: "fixture".to_string(),
                        sha: p.sha.clone(),
                        subject: p.subject.clone(),
                        visible: true,
                        caption: None,
                        confidence: Confidence::Trailer,
                        via: "commit-trailer".to_string(),
                        attributed_here: true,
                    },
                    Some(other) => panic!("test fixture has one repo (index 0), got {other}"),
                })
                .collect(),
            None,
        );
        assert_eq!(lane.total, 3);
        assert_eq!(lane.shown, 3);
        assert!(!lane.truncated);
        assert!(!lane.items[0].visible);
        assert_eq!(
            lane.items[0].caption.as_deref(),
            Some("repo not configured here")
        );
        assert_eq!(lane.items[0].confidence, Confidence::None);
        assert_eq!(lane.items[0].via, "unmirrored");
        assert_eq!(lane.items[0].repo, "");
        assert!(!lane.items[1].visible);
        assert_eq!(lane.items[1].via, "unmirrored");
        assert!(lane.items[2].visible);
        assert_eq!(lane.items[2].repo, "fixture");
    }

    #[test]
    fn commits_lane_caps_at_max_commits_and_says_so() {
        let items: Vec<CommitItem> = (0..25)
            .map(|i| CommitItem {
                repo: "fixture".to_string(),
                sha: format!("{i:040}"),
                subject: None,
                visible: true,
                caption: None,
                confidence: Confidence::Exact,
                via: "by-commit".to_string(),
                attributed_here: true,
            })
            .collect();
        let lane = cap_commits(items, None);
        assert_eq!(lane.total, 25);
        assert_eq!(lane.shown, MAX_COMMITS);
        assert!(lane.truncated);
        assert_eq!(lane.items.len(), MAX_COMMITS);
        // First 20 kept, in order.
        assert_eq!(lane.items[0].sha, format!("{:040}", 0));
        assert_eq!(lane.items[19].sha, format!("{:040}", 19));
    }

    fn attribution(session_id: Option<&str>) -> Attribution {
        Attribution {
            schema: "join/1",
            confidence: Confidence::Exact,
            via: "by-commit".to_string(),
            session_id: session_id.map(str::to_string),
            kb: None,
            display_name: None,
            started_at: None,
            sha: SHA_A.to_string(),
        }
    }

    #[test]
    fn attributed_here_false_when_ladder_names_another_session() {
        let here = commit_item_from_attribution(
            SID,
            "fixture".to_string(),
            SHA_A.to_string(),
            None,
            &attribution(Some(SID)),
        );
        assert!(here.attributed_here);
        let other = commit_item_from_attribution(
            SID,
            "fixture".to_string(),
            SHA_A.to_string(),
            None,
            &attribution(Some("sess-other")),
        );
        assert!(!other.attributed_here);
        let unknown = commit_item_from_attribution(
            SID,
            "fixture".to_string(),
            SHA_A.to_string(),
            None,
            &attribution(None),
        );
        assert!(!unknown.attributed_here);
    }

    fn insert_finding(
        store: &Store,
        review_id: i64,
        repo_id: i64,
        slug: &str,
        severity: &str,
        annotation_author: &str,
        finding_author: Option<&str>,
    ) {
        let finding = NewReviewFinding {
            review_id,
            repo_id,
            ps_number: 1,
            slug: slug.to_string(),
            severity: severity.to_string(),
            category: "Style".to_string(),
            location_kind: LOCATION_KIND_SINGLE.to_string(),
            location_path: "src/lib.rs".to_string(),
            location_lines: None,
            location_removed: false,
            title: "fixture finding".to_string(),
            rationale: "fixture rationale".to_string(),
            recommendation: None,
            evidence_lang: None,
            evidence_source: None,
            anchor_kind: "line".to_string(),
            anchor: serde_json::to_string(&anchor_for_line(1, "x")).unwrap(),
            anchor2: None,
            side: Some("new".to_string()),
            author: annotation_author.to_string(),
            import_batch_id: "fixture".to_string(),
            origin: FINDING_ORIGIN_MANUAL.to_string(),
            finding_author: finding_author.map(str::to_string),
            act: "issue".to_string(),
            blocking: false,
            cites_json: None,
            fingerprint: None,
        };
        store
            .insert_review_finding(&finding, 1_700_000_000)
            .unwrap();
    }

    #[test]
    fn findings_lane_uses_the_shared_session_author_prefix() {
        // The author string is built from the shared constant, never
        // hardcoded — the test proves the constant's VALUE is what the SQL
        // counted under.
        assert_eq!(behavioral::SESSION_AUTHOR_PREFIX, "session:");
        let (_tmp, store) = open_temp();
        let repo_id = store.upsert_repo("fixture", "/repos/fixture").unwrap();
        let review_id = store
            .create_review(
                "fixture",
                Some("fixture"),
                "main",
                "feature",
                None,
                1_700_000_000,
            )
            .unwrap();

        // One blocker via finding_author (review_findings.author), one via
        // annotations.author (the linked thread identity).
        insert_finding(
            &store,
            review_id,
            repo_id,
            "f-a",
            SEVERITY_BLOCKER,
            "someone-else",
            Some(&format!("{}{}", behavioral::SESSION_AUTHOR_PREFIX, SID)),
        );
        insert_finding(
            &store,
            review_id,
            repo_id,
            "f-b",
            SEVERITY_CONCERN,
            &format!("{}{}", behavioral::SESSION_AUTHOR_PREFIX, SID),
            None,
        );
        // A different author never counts.
        insert_finding(
            &store,
            review_id,
            repo_id,
            "f-c",
            SEVERITY_CONCERN,
            "claude",
            Some("claude"),
        );
        // A superseded finding never counts either.
        insert_finding(
            &store,
            review_id,
            repo_id,
            "f-d",
            SEVERITY_OK,
            &format!("{}{}", behavioral::SESSION_AUTHOR_PREFIX, SID),
            Some(&format!("{}{}", behavioral::SESSION_AUTHOR_PREFIX, SID)),
        );
        let db_path = _tmp.path().join("index.db");
        drop(store);
        let conn = rusqlite::Connection::open(&db_path).unwrap();
        conn.execute(
            "UPDATE review_findings SET superseded = 1 WHERE slug = 'f-d'",
            [],
        )
        .unwrap();
        drop(conn);
        let store = Store::open(&db_path).unwrap();

        let author = format!("{}{}", behavioral::SESSION_AUTHOR_PREFIX, SID);
        let counts = store.finding_severity_counts_authored_by(&author).unwrap();
        assert_eq!(counts.blocker, 1);
        assert_eq!(counts.concern, 1);
        assert_eq!(counts.ok, 0, "superseded must not count");
        let lane = findings_lane(&counts);
        assert_eq!(lane.basis, "author");
        assert_eq!(lane.total, 2);

        // The literal "claude" author counts for nobody.
        let other = store.finding_severity_counts_authored_by("claude").unwrap();
        assert_eq!(other.blocker + other.concern + other.ok, 1);
    }

    fn key_set(v: &serde_json::Value, out: &mut std::collections::BTreeSet<String>) {
        match v {
            serde_json::Value::Object(map) => {
                for (k, val) in map {
                    out.insert(k.clone());
                    key_set(val, out);
                }
            }
            serde_json::Value::Array(arr) => {
                for item in arr {
                    key_set(item, out);
                }
            }
            _ => {}
        }
    }

    #[test]
    fn card_serialization_has_no_path_or_text_fields() {
        let card = SessionCardOut {
            schema: SCHEMA,
            sid: SID.to_string(),
            now_unix: 1,
            commits: cap_commits(
                vec![CommitItem {
                    repo: "fixture".to_string(),
                    sha: SHA_A.to_string(),
                    subject: Some("subject line".to_string()),
                    visible: true,
                    caption: None,
                    confidence: Confidence::Trailer,
                    via: "commit-trailer".to_string(),
                    attributed_here: true,
                }],
                None,
            ),
            reviews: reviews_lane(vec![ReviewItem {
                id: 7,
                repo: "fixture".to_string(),
                title: "Add fixture review".to_string(),
                state: "open".to_string(),
                verdict: Some("approve".to_string()),
                verdict_ps: Some(2),
                pr_number: Some(12),
                updated_at: 1_700_000_000,
                basis: "session_id".to_string(),
            }]),
            findings: findings_lane(&SeverityCounts {
                blocker: 1,
                concern: 2,
                ok: 1,
            }),
            presence: PresenceLane {
                available: true,
                caption: None,
                files: 2,
                newest_age_secs: 40,
            },
        };
        let value = serde_json::to_value(&card).unwrap();
        let mut keys = std::collections::BTreeSet::new();
        key_set(&value, &mut keys);
        for banned in ["path", "rel", "body", "text", "prompt"] {
            assert!(
                !keys.contains(banned),
                "key set must not contain {banned:?}: {keys:?}"
            );
        }
    }

    #[test]
    fn card_matches_the_committed_golden() {
        let card = SessionCardOut {
            schema: SCHEMA,
            sid: SID.to_string(),
            now_unix: 1_700_000_000,
            commits: cap_commits(
                vec![
                    commit_item_from_attribution(
                        SID,
                        "fixture".to_string(),
                        SHA_A.to_string(),
                        Some("fixture commit one".to_string()),
                        &Attribution {
                            schema: "join/1",
                            confidence: Confidence::Trailer,
                            via: "commit-trailer".to_string(),
                            session_id: Some(SID.to_string()),
                            kb: None,
                            display_name: None,
                            started_at: None,
                            sha: SHA_A.to_string(),
                        },
                    ),
                    commit_item_from_attribution(
                        SID,
                        "fixture".to_string(),
                        SHA_B.to_string(),
                        Some("fixture commit two".to_string()),
                        &Attribution {
                            schema: "join/1",
                            confidence: Confidence::Exact,
                            via: "by-commit".to_string(),
                            session_id: Some(SID.to_string()),
                            kb: None,
                            display_name: None,
                            started_at: None,
                            sha: SHA_B.to_string(),
                        },
                    ),
                    commit_item_not_visible(
                        "fixture".to_string(),
                        SHA_C.to_string(),
                        Some("fixture commit three".to_string()),
                        "commit not in this checkout",
                        "not-in-checkout",
                    ),
                ],
                None,
            ),
            reviews: reviews_lane(vec![ReviewItem {
                id: 7,
                repo: "fixture".to_string(),
                title: "Add fixture review".to_string(),
                state: "open".to_string(),
                verdict: Some("approve".to_string()),
                verdict_ps: Some(2),
                pr_number: Some(12),
                updated_at: 1_700_000_000,
                basis: "session_id".to_string(),
            }]),
            findings: findings_lane(&SeverityCounts {
                blocker: 1,
                concern: 2,
                ok: 1,
            }),
            presence: PresenceLane {
                available: true,
                caption: None,
                files: 2,
                newest_age_secs: 40,
            },
        };
        let mut got = serde_json::to_value(&card).unwrap();
        let mut want: serde_json::Value =
            serde_json::from_str(include_str!("../../../tests/fixtures/session-card-v1.json"))
                .unwrap();
        // `now_unix` is the only permitted difference between reads.
        got["now_unix"] = serde_json::Value::Null;
        want["now_unix"] = serde_json::Value::Null;
        assert_eq!(got, want);
    }
}
