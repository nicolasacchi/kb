//! PRR-R8 ("The PR Room," kb v0.39 T2, design-addendum-2 §B) — `POST
//! /api/reviews/sweep`: the stale-backlog sweep. Walks every PR-bound
//! review (default `state=open`) and reconciles it against LIVE GitHub, so
//! a stale approval / moved head / red check / closed-out PR is surfaced
//! without anyone remembering to check `pr-status` one review at a time.
//!
//! # Route
//!
//! **Loopback-only** — this route REFRESHES stored `pr_meta_json`/
//! `pr_head_sha`/`pr_meta_fetched_at` snapshots (a write), unlike `GET
//! /reviews/{id}/pr-status` (PRR-R4, `reviews::pr_status_route`) which only
//! READS live GitHub without persisting anything. Same loopback-only
//! review-mutation family as `POST /reviews/pr`/`POST /reviews/gc`.
//!
//! # Per-row degrade, never abort the sweep
//!
//! Every review is swept independently — one PR's GitHub call failing
//! (rate limit, network, bad origin) degrades ONLY that row's
//! `unavailable_reason` (same shape [`reviews::pr_status_route`] already
//! established) and never aborts the rest of the batch. Owner/repo is
//! resolved FRESH per review via `github::github_repo` (never parsed back
//! out of the stored `pr_repo_slug`) — same rationale
//! `pr_status_route`'s own doc gives.
//!
//! # `checks` — a 4th `warn` bucket beyond the addendum's literal 3
//!
//! `github::normalize_check_status` reports FOUR values
//! (`pass|fail|warn|pending` — `warn` covers a `neutral`/`skipped`/`stale`/
//! unknown conclusion), not the three the addendum's row sketch names.
//! Rather than silently folding `warn` into one of the other three (which
//! would misrepresent either a real failure or a real pass), this route
//! surfaces it as its own named field — a deliberate, documented deviation
//! from the addendum's literal shape, flagged here and in this unit's own
//! report.
//!
//! # `head_drift` / `new_head_commits` — what they compare
//!
//! `head_drift` compares the FRESH `get_pull` head sha against the
//! PREVIOUSLY STORED `pr_head_sha` (the snapshot this very call is about to
//! refresh) — "did the PR move since kb-code last looked," not
//! `pr_status_route`'s own local-patchset comparison (a different
//! question: "does my latest local capture match the live PR"). \
//! `new_head_commits` is `git rev-list --count <old_head>..<new_head>`,
//! computed ONLY when both shas are already resolvable in the local repo
//! (same "local rev-list when the pr ref is present" ask, and the same
//! silent-degrade-to-`None`-on-unresolvable posture `pr_status_route`'s own
//! `commits_behind` uses — the live call itself succeeded, so this is not
//! an `unavailable_reason`).
//!
//! # `suggest_close` — never auto-closed
//!
//! `true` iff the live PR is `merged` or `state == "closed"` while the
//! local review record is still `state == "open"` — purely informational;
//! nothing in this route ever writes `reviews.state`. The operator/agent
//! decides whether to `review close`.
//!
//! # `verdict_stale` / `unanswered_questions` — reused, not reimplemented
//!
//! Both are LOCAL-only (no GitHub call) and computed via the SAME
//! functions `review_inbox`'s attention queue already uses
//! (`reviews::verdict_block`, `review_inbox::thread_is_unanswered` +ن its
//! surrounding question/reply grouping) — so a review's sweep row can never
//! disagree with its own inbox row on these two terms.

use crate::reviews::{self, ReviewGitError};
use crate::routes::{find_repo, ApiError};
use crate::state::SharedState;
use crate::store::{self, ReviewPrBinding, ReviewRow, StoreBlocking};
use axum::extract::State;
use axum::http::{header, StatusCode};
use axum::response::IntoResponse;
use axum::Json;
use futures::stream::{self, StreamExt};
use serde::Deserialize;
use std::collections::HashMap;

pub const SCHEMA: &str = "review-sweep/1";

/// Bounded per-sweep concurrency (design-addendum-2 §B: "small bounded
/// concurrency, e.g. 4").
pub const SWEEP_CONCURRENCY: usize = 4;

fn now_unix() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

#[derive(Debug, Deserialize, Default)]
pub struct SweepBody {
    #[serde(default)]
    pub repo: Option<String>,
    #[serde(default)]
    pub all_repos: bool,
    #[serde(default)]
    pub include_closed: bool,
}

/// One review's sweep outcome — the row plus, when its stored snapshot
/// actually changed, the `(review_id, repo)` this call should emit
/// `review.changed{reason:"pr_refreshed"}` for. Kept separate from the row
/// itself so `sweep_route` can emit AFTER every row has been computed
/// (deterministic response ordering first, side effects second).
struct SweepOutcome {
    row: serde_json::Value,
    emit: Option<(i64, String)>,
}

/// See the module doc's "verdict_stale / unanswered_questions" section —
/// identical derivation to `review_inbox::list_inbox_route`'s own, just
/// extracted so `sweep_one` can call it without re-deriving the grouping.
///
/// 2026-08-31 incident (store.rs module doc): takes `&Store` (not
/// `&SharedState`, its only prior use) so `sweep_one` wraps the whole
/// thing in ONE `run_blocking` closure.
fn local_only_fields(store: &store::Store, review: &ReviewRow) -> Result<(bool, i64), ApiError> {
    let latest_ps = store.latest_patchset(review.id)?;
    let (_verdict, verdict_stale) =
        reviews::verdict_block(review, latest_ps.as_ref().map(|p| p.ps_number));

    let ann_rows = store.list_review_annotations(review.id, true)?;
    let mut by_id: HashMap<String, store::AnnotationRow> = HashMap::new();
    let mut replies_by_parent: HashMap<String, Vec<store::AnnotationRow>> = HashMap::new();
    for row in ann_rows {
        match row.parent_id.clone() {
            Some(pid) => replies_by_parent.entry(pid).or_default().push(row),
            None => {
                by_id.insert(row.id.clone(), row);
            }
        }
    }
    let mut unanswered_questions = 0i64;
    for (aid, ann) in &by_id {
        if ann.intent != crate::annotations::INTENT_QUESTION || ann.resolved {
            continue;
        }
        let replies: Vec<&store::AnnotationRow> = replies_by_parent
            .get(aid)
            .map(|v| v.iter().collect())
            .unwrap_or_default();
        if crate::review_inbox::thread_is_unanswered(&ann.author, &replies) {
            unanswered_questions += 1;
        }
    }
    Ok((verdict_stale, unanswered_questions))
}

/// `{pass, fail, warn, pending}` — see the module doc's "checks" section
/// for why `warn` exists beyond the addendum's literal 3-field sketch.
fn checks_summary(checks: &[crate::github::CheckRunOut]) -> serde_json::Value {
    let mut pass = 0i64;
    let mut fail = 0i64;
    let mut warn = 0i64;
    let mut pending = 0i64;
    for c in checks {
        match c.status.as_str() {
            "pass" => pass += 1,
            "fail" => fail += 1,
            "warn" => warn += 1,
            _ => pending += 1,
        }
    }
    serde_json::json!({ "pass": pass, "fail": fail, "warn": warn, "pending": pending })
}

/// One review's full sweep — the live half. Never returns `Err` for a
/// GitHub-side failure (degrades the row's `unavailable_reason` instead);
/// `Err` here means something LOCAL and unexpected (a store error, a
/// panicked blocking task) — the caller drops the whole sweep response on
/// that (matches every other route's `?`-through-`ApiError` convention;
/// unlike a GitHub failure, a local store error is not this review's own
/// fault to isolate).
async fn sweep_one(
    state: SharedState,
    review: ReviewRow,
    binding: ReviewPrBinding,
) -> Result<SweepOutcome, ApiError> {
    let pr_number = binding
        .pr_number
        .expect("caller pre-filters to PR-bound reviews");

    let review_for_local = review.clone();
    let (verdict_stale, unanswered_questions) = state
        .store
        .run_blocking(move |store| local_only_fields(store, &review_for_local))
        .await?;

    let repo_root = {
        let (repo_entry, _repo_id) = find_repo(&state, &review.repo)?;
        repo_entry.path.clone()
    };

    let root_for_origin = repo_root.clone();
    let gh_repo_result =
        tokio::task::spawn_blocking(move || crate::github::github_repo(&root_for_origin))
            .await
            .map_err(|e| {
                ApiError::new(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    format!("origin lookup task panicked: {e}"),
                )
            })?;

    let gh = match gh_repo_result {
        Ok(gh) => gh,
        Err(e) => {
            return Ok(SweepOutcome {
                row: unavailable_row(
                    &review,
                    pr_number,
                    verdict_stale,
                    unanswered_questions,
                    &e.to_string(),
                ),
                emit: None,
            })
        }
    };

    let pull = match state
        .github
        .get_pull(&gh.owner, &gh.name, pr_number as u64)
        .await
    {
        Ok(p) => p,
        Err(e) => {
            return Ok(SweepOutcome {
                row: unavailable_row(
                    &review,
                    pr_number,
                    verdict_stale,
                    unanswered_questions,
                    &e.to_string(),
                ),
                emit: None,
            })
        }
    };

    // checks + review_decision are SUB-parts of an already-successful
    // `get_pull` — a failure here degrades that ONE field to `null`
    // silently (same "checks failing doesn't sink an otherwise-successful
    // PR fetch" precedent `create_review_pr` established), never the
    // row's `unavailable_reason` (that's reserved for "the live call
    // itself failed" — see the module doc).
    let (checks_res, reviews_res) = tokio::join!(
        state
            .github
            .list_checks(&gh.owner, &gh.name, &pull.head_sha),
        state
            .github
            .list_reviews(&gh.owner, &gh.name, pr_number as u64),
    );
    let checks_list = checks_res.as_ref().ok().cloned().unwrap_or_default();
    let checks_json = checks_res.as_ref().ok().map(|c| checks_summary(c));
    let review_decision = reviews_res.ok().and_then(|r| r.review_decision);

    let pr_state = if pull.merged {
        "merged".to_string()
    } else {
        pull.state.clone()
    };
    let old_head = binding.pr_head_sha.clone();
    let head_drift = old_head.as_deref() != Some(pull.head_sha.as_str());

    let new_head_commits = if head_drift {
        match &old_head {
            Some(old) => {
                let root2 = repo_root.clone();
                let old2 = old.clone();
                let new2 = pull.head_sha.clone();
                let resolved: Result<(String, String), ReviewGitError> =
                    tokio::task::spawn_blocking(move || {
                        let o =
                            reviews::resolve_commit_sha(&root2, &reviews::parse_user_ref(&old2)?)?;
                        let n =
                            reviews::resolve_commit_sha(&root2, &reviews::parse_user_ref(&new2)?)?;
                        Ok((o, n))
                    })
                    .await
                    .map_err(|e| ApiError::new(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
                match resolved {
                    Ok((o, n)) => {
                        let root3 = repo_root.clone();
                        let count = tokio::task::spawn_blocking(move || {
                            reviews::commit_count(&root3, &o, &n)
                        })
                        .await
                        .map_err(|e| {
                            ApiError::new(StatusCode::INTERNAL_SERVER_ERROR, e.to_string())
                        })?;
                        count.ok().map(|n| n as i64)
                    }
                    // Neither sha locally resolvable yet — silent `None`,
                    // NOT an `unavailable_reason` (the live call succeeded;
                    // see the module doc).
                    Err(_) => None,
                }
            }
            None => None,
        }
    } else {
        Some(0)
    };

    let suggest_close = review.state == "open" && (pull.merged || pull.state == "closed");

    // --- persist the refreshed snapshot -------------------------------------
    let meta = serde_json::json!({
        "title": pull.title,
        "author": pull.author,
        "head_ref": pull.head_ref,
        "base_ref": pull.base_ref,
        "draft": pull.draft,
        "state": pull.state,
        "merged": pull.merged,
        "labels": pull.labels,
        "merge_state_status": pull.merge_state_status,
        // V70-A3X — kept in lock-step with `reviews::create_review_pr`'s
        // own `meta` snapshot.
        "body": pull.body,
        "checks": checks_list.into_iter().take(crate::github::MAX_CHECKS).collect::<Vec<_>>(),
    });
    let meta_str = serde_json::to_string(&meta)
        .map_err(|e| ApiError::new(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    let changed = old_head.as_deref() != Some(pull.head_sha.as_str())
        || binding.pr_meta_json.as_deref() != Some(meta_str.as_str());

    let now = now_unix();
    let review_id = review.id;
    let head_sha = pull.head_sha.clone();
    let meta_str_c = meta_str.clone();
    state
        .store
        .run_blocking(move |store| {
            store.set_review_pr_meta(review_id, Some(&head_sha), Some(&meta_str_c), now)
        })
        .await?;

    let row = serde_json::json!({
        "review_id": review.id,
        "pr_number": pr_number,
        "pr_state": pr_state,
        "head_drift": head_drift,
        "new_head_commits": new_head_commits,
        "review_decision": review_decision,
        "checks": checks_json,
        "verdict_stale": verdict_stale,
        "unanswered_questions": unanswered_questions,
        "suggest_close": suggest_close,
        "unavailable_reason": serde_json::Value::Null,
    });

    Ok(SweepOutcome {
        row,
        emit: changed.then(|| (review.id, review.repo.clone())),
    })
}

fn unavailable_row(
    review: &ReviewRow,
    pr_number: i64,
    verdict_stale: bool,
    unanswered_questions: i64,
    reason: &str,
) -> serde_json::Value {
    serde_json::json!({
        "review_id": review.id,
        "pr_number": pr_number,
        "pr_state": serde_json::Value::Null,
        "head_drift": serde_json::Value::Null,
        "new_head_commits": serde_json::Value::Null,
        "review_decision": serde_json::Value::Null,
        "checks": serde_json::Value::Null,
        "verdict_stale": verdict_stale,
        "unanswered_questions": unanswered_questions,
        "suggest_close": false,
        "unavailable_reason": reason,
    })
}

/// `POST /api/reviews/sweep` (design-addendum-2 §B). LOOPBACK-ONLY. See
/// the module doc for the per-row degrade / concurrency / persistence
/// contract.
pub async fn sweep_route(
    State(state): State<SharedState>,
    Json(body): Json<SweepBody>,
) -> Result<impl IntoResponse, ApiError> {
    if body.repo.is_some() == body.all_repos {
        return Err(ApiError::bad_request(
            "pass exactly one of `repo` (a configured repo name) or `all_repos: true`",
        ));
    }
    let repo_names: Vec<String> = match body.repo.as_deref() {
        Some(r) => {
            find_repo(&state, r)?;
            vec![r.to_string()]
        }
        None => state.repos.iter().map(|r| r.name.clone()).collect(),
    };
    let state_filter: Option<&str> = if body.include_closed {
        None
    } else {
        Some("open")
    };

    // 2026-08-31 incident (store.rs module doc): the whole repo×review
    // scan is pure store work — one blocking-pool trip instead of one
    // round trip per (repo, review) pair.
    let state_filter_owned = state_filter.map(|s| s.to_string());
    let repo_names_c = repo_names.clone();
    let targets: Vec<(ReviewRow, ReviewPrBinding)> = state
        .store
        .run_blocking(move |store| -> Result<_, ApiError> {
            let mut targets = Vec::new();
            for name in &repo_names_c {
                for review in store.list_reviews(name, state_filter_owned.as_deref())? {
                    let binding = store.get_review_pr_binding(review.id)?.unwrap_or_default();
                    if binding.pr_number.is_some() {
                        targets.push((review, binding));
                    }
                }
            }
            Ok(targets)
        })
        .await?;

    let outcomes: Vec<Result<SweepOutcome, ApiError>> =
        stream::iter(targets.into_iter().map(|(review, binding)| {
            let state = state.clone();
            async move { sweep_one(state, review, binding).await }
        }))
        .buffer_unordered(SWEEP_CONCURRENCY)
        .collect()
        .await;

    let mut rows = Vec::with_capacity(outcomes.len());
    let mut to_emit: Vec<(i64, String)> = Vec::new();
    for outcome in outcomes {
        let outcome = outcome?;
        if let Some(target) = outcome.emit {
            to_emit.push(target);
        }
        rows.push(outcome.row);
    }
    // Deterministic response ordering regardless of `buffer_unordered`
    // completion order — same "the query result isn't naturally
    // deterministic, so the layer above imposes one" precedent as
    // `sort_inbox_rows`/`Store::recurrence_pairs`.
    rows.sort_by(|a, b| {
        a["review_id"]
            .as_i64()
            .unwrap_or(0)
            .cmp(&b["review_id"].as_i64().unwrap_or(0))
    });
    to_emit.sort_by_key(|(review_id, _)| *review_id);

    for (review_id, repo) in &to_emit {
        reviews::emit_review_changed(&state.bus, *review_id, repo, "pr_refreshed", false);
    }

    let refreshed = to_emit.len();
    let unavailable = rows
        .iter()
        .filter(|r| !r["unavailable_reason"].is_null())
        .count();
    let suggest_close = rows
        .iter()
        .filter(|r| r["suggest_close"].as_bool().unwrap_or(false))
        .count();

    Ok((
        [(header::CACHE_CONTROL, "no-store")],
        Json(serde_json::json!({
            "schema": SCHEMA,
            "summary": {
                "swept": rows.len(),
                "refreshed": refreshed,
                "unavailable": unavailable,
                "suggest_close": suggest_close,
            },
            "rows": rows,
        })),
    ))
}
