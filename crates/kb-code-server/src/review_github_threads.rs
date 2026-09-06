//! PRR-R7 ("The PR Room," kb v0.39, GitHub thread import — one conversation
//! timeline) — `GET /api/reviews/{id}/github-threads`. Spec of record:
//! `/tmp/design-addendum-2.md` §A.
//!
//! # GitHub stays the source of truth
//!
//! Every response is LIVE-composed per request from `github::GithubClient::
//! list_pull_comments` — nothing here is ever persisted (no new table, no
//! cached snapshot). A live GitHub failure degrades to an `unavailable_reason`
//! string, HTTP 200, and an empty `threads` list — the SAME posture
//! `github.rs`'s own module doc documents for `GET /api/prs`/`GET
//! /api/prs/{n}/comments` ("never a 5xx for GitHub had a bad day"); a
//! non-GitHub origin still 400s BEFORE any network call, same split as
//! those routes.
//!
//! # Position mapping — the SAME ladder, never a second one
//!
//! Each inline (path-carrying) comment's anchored line is derived from its
//! `diff_hunk`'s LAST line (the +/-/space prefix stripped, [`last_diff_hunk_line`])
//! and re-resolved against the review's LATEST patchset via the identical
//! exact→fuzzy→snippet-guard ladder local comment carry-forward uses:
//! `annotations::anchor_for_line` builds the SAME `Anchor::Selection` shape
//! `routes::create_annotation` does, `annotations::resolve` is the SAME
//! resolver `review_comments::resolve_for_ps_with_content` calls, and
//! `review_comments::line_matches_snippet` is the SAME snippet guard. A miss
//! at any step (no diff_hunk, no line number, the path absent at the target
//! blob, a stale resolution, or a resolved line whose text no longer
//! matches the snippet) is an honest `orphaned: true` — never a guessed
//! line. An issue-style (path-less) comment is always `general: true`.
//!
//! # Thread grouping
//!
//! GitHub's `in_reply_to_id` always names the THREAD ROOT (never a chained
//! immediate parent — verified against GitHub's own documented behavior),
//! so one level of nesting is exact, not an approximation. A reply whose
//! named parent isn't present in this same (capped) page — a truncated
//! page, or a genuinely dangling reference — is promoted to its own root
//! rather than silently dropped.

use crate::annotations;
use crate::github::PrCommentOut;
use crate::review_comments;
use crate::reviews::require_review;
use crate::routes::ApiError;
use crate::state::SharedState;
use crate::store::{ReviewPatchsetRow, StoreBlocking};
use axum::extract::{Path as AxumPath, State};
use axum::http::{header, StatusCode};
use axum::response::IntoResponse;
use axum::Json;
use kb_core::review::Anchor;
use std::collections::HashMap;
use std::path::Path;

pub const SCHEMA: &str = "kbc-github-threads/1";

/// `GET /api/reviews/{id}/github-threads` — bearer (see the module doc).
/// `400` when the review isn't PR-bound or has zero patchsets (same "no
/// target to resolve against" rule `pr_status_route`/`put_verdict` enforce).
pub async fn github_threads_route(
    State(state): State<SharedState>,
    AxumPath(id): AxumPath<i64>,
) -> Result<impl IntoResponse, ApiError> {
    let (review, repo, _repo_id) = require_review(&state, id).await?;
    // 2026-08-31 incident (store.rs module doc): two separate blocking-pool
    // trips (not merged) so the pr_number 400 still short-circuits BEFORE
    // the latest_patchset lookup, byte-identical to the pre-fix ordering.
    let binding = state
        .store
        .run_blocking(move |store| store.get_review_pr_binding(id))
        .await?
        .ok_or_else(|| ApiError::not_found(format!("no such review: {id}")))?;
    let Some(pr_number) = binding.pr_number else {
        return Err(ApiError::bad_request(format!(
            "review {id} is not bound to a PR"
        )));
    };
    let latest_ps = state
        .store
        .run_blocking(move |store| store.latest_patchset(id))
        .await?
        .ok_or_else(|| ApiError::bad_request(format!("review {id} has no patchsets")))?;

    // Owner/repo resolved FRESH each call — same rationale as
    // `pr_status_route`'s own doc (never parsed back out of the stored,
    // possibly-`"unknown"` `pr_repo_slug`). A non-GitHub origin 400s here,
    // BEFORE any network call — resolved via `From<GithubError> for
    // ApiError`, same as every other GitHub-overlay route.
    let root_for_origin = repo.path.clone();
    let gh_repo = tokio::task::spawn_blocking(move || crate::github::github_repo(&root_for_origin))
        .await
        .map_err(|e| {
            ApiError::new(
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("origin lookup task panicked: {e}"),
            )
        })??;

    // V70-A3X: `list_pull_comments` now itself enforces `MAX_COMMENTS` and
    // reports honestly whether more existed — no further truncation
    // needed here.
    let (threads, truncated, unavailable_reason) = match state
        .github
        .list_pull_comments(&gh_repo.owner, &gh_repo.name, pr_number as u64)
        .await
    {
        Ok((list, truncated)) => {
            let threads = build_threads(&repo.path, &latest_ps, &list);
            (threads, truncated, None)
        }
        Err(e) => (Vec::new(), false, Some(e.to_string())),
    };

    Ok((
        [(header::CACHE_CONTROL, "no-store")],
        Json(serde_json::json!({
            "schema": SCHEMA,
            "review_id": id,
            "repo": review.repo,
            "pr_number": pr_number,
            "ps": latest_ps.ps_number,
            "threads": threads,
            "truncated": truncated,
            "fetched_at": chrono::Utc::now().timestamp(),
            "unavailable_reason": unavailable_reason,
        })),
    ))
}

/// Group `comments` (already capped at [`crate::github::MAX_COMMENTS`])
/// into top-level threads with nested replies — see the module doc for the
/// `in_reply_to_id`-names-the-root contract. Each ROOT carries its
/// [`position_for`] result; replies are conversation under an already-
/// positioned thread and carry no position of their own.
fn build_threads(
    repo_root: &Path,
    latest_ps: &ReviewPatchsetRow,
    comments: &[PrCommentOut],
) -> Vec<serde_json::Value> {
    let idx_by_id: HashMap<u64, usize> = comments
        .iter()
        .enumerate()
        .filter_map(|(i, c)| c.id.map(|cid| (cid, i)))
        .collect();

    // A comment is a REPLY iff `in_reply_to` names a DIFFERENT, PRESENT
    // comment in this same page. A missing/self-referential/dangling
    // target is never a reply — see the module doc.
    let parent_of = |i: usize| -> Option<usize> {
        let pid = comments[i].in_reply_to?;
        let pi = *idx_by_id.get(&pid)?;
        if pi == i {
            None
        } else {
            Some(pi)
        }
    };

    let mut replies_of: HashMap<usize, Vec<usize>> = HashMap::new();
    for i in 0..comments.len() {
        if let Some(pi) = parent_of(i) {
            replies_of.entry(pi).or_default().push(i);
        }
    }

    // Blob-read cache, same `(path, sha)` convention
    // `review_comments::build_comment_groups` uses — a page can carry many
    // inline comments on the same file/side.
    let mut cache: HashMap<(String, String), Option<String>> = HashMap::new();

    let mut threads = Vec::with_capacity(comments.len());
    for (i, c) in comments.iter().enumerate() {
        if parent_of(i).is_some() {
            continue;
        }
        let mut obj = comment_json(c);
        attach_position(&mut obj, position_for(repo_root, latest_ps, c, &mut cache));
        let replies: Vec<serde_json::Value> = replies_of
            .remove(&i)
            .unwrap_or_default()
            .into_iter()
            .map(|ri| comment_json(&comments[ri]))
            .collect();
        obj["replies"] = serde_json::Value::Array(replies);
        threads.push(obj);
    }
    threads
}

fn comment_json(c: &PrCommentOut) -> serde_json::Value {
    serde_json::json!({
        "id": c.id,
        "author": c.author,
        "body": c.body,
        "created_at": c.created_at,
        "html_url": c.html_url,
        "path": c.path,
        "side": c.side,
        "line": c.line,
        "original_line": c.original_line,
        "in_reply_to": c.in_reply_to,
    })
}

/// The three possible outcomes of [`position_for`] — see the module doc.
enum Position {
    General,
    Resolved { line: u32, confidence: &'static str },
    Orphaned,
}

/// Splice `position` onto `obj` per the design doc's wire shape: exactly
/// ONE of `general: true` | `orphaned: true` | `resolved: {line, confidence}`.
fn attach_position(obj: &mut serde_json::Value, position: Position) {
    match position {
        Position::General => obj["general"] = serde_json::Value::Bool(true),
        Position::Orphaned => obj["orphaned"] = serde_json::Value::Bool(true),
        Position::Resolved { line, confidence } => {
            obj["resolved"] = serde_json::json!({ "line": line, "confidence": confidence });
        }
    }
}

/// Position-map one comment onto `latest_ps` — see the module doc.
fn position_for(
    repo_root: &Path,
    latest_ps: &ReviewPatchsetRow,
    c: &PrCommentOut,
    cache: &mut HashMap<(String, String), Option<String>>,
) -> Position {
    let Some(path) = c.path.as_deref() else {
        return Position::General;
    };
    let Some(last_line) = last_diff_hunk_line(c.diff_hunk.as_deref()) else {
        return Position::Orphaned;
    };
    let Some(offset) = c.line.or(c.original_line) else {
        return Position::Orphaned;
    };
    let offset = offset.min(u64::from(u32::MAX)) as u32;

    // GitHub's `side`: "LEFT" (old file) | "RIGHT" (new file, the default
    // when absent) -> `review_comments::target_sha_for_side`'s own "old" /
    // anything-else convention.
    let mapped_side = match c.side.as_deref() {
        Some("LEFT") => "old",
        _ => "new",
    };
    let sha = review_comments::target_sha_for_side(Some(mapped_side), latest_ps).to_string();

    let key = (path.to_string(), sha.clone());
    if !cache.contains_key(&key) {
        let text = review_comments::read_blob_text(repo_root, path, &sha);
        cache.insert(key.clone(), text);
    }
    let Some(content) = cache.get(&key).and_then(|c| c.as_deref()) else {
        return Position::Orphaned;
    };

    let anchor = annotations::anchor_for_line(offset, last_line);
    let snippet = match &anchor {
        Anchor::Selection { snippet, .. } => snippet.as_str(),
        _ => "",
    };
    let resolved = annotations::resolve(content, &anchor);
    if resolved.stale || !review_comments::line_matches_snippet(content, resolved.line, snippet) {
        return Position::Orphaned;
    }
    Position::Resolved {
        line: resolved.line,
        confidence: review_comments::confidence_str(resolved.confidence),
    }
}

/// The diff_hunk's LAST line, with its leading `+`/`-`/` ` prefix stripped.
/// `None` (an honest orphan upstream) for: no hunk at all, an empty hunk, a
/// hunk whose only line is the `@@ ... @@` header (no body to anchor on),
/// or a last line with none of the three recognised prefixes (GitHub always
/// prefixes hunk BODY lines with one of them — an unparseable line is
/// treated as a malformed hunk, never guessed at).
fn last_diff_hunk_line(diff_hunk: Option<&str>) -> Option<&str> {
    let hunk = diff_hunk?;
    let last = hunk.lines().next_back()?;
    if last.is_empty() || last.starts_with("@@") {
        return None;
    }
    last.strip_prefix('+')
        .or_else(|| last.strip_prefix('-'))
        .or_else(|| last.strip_prefix(' '))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn last_diff_hunk_line_strips_the_plus_prefix() {
        let hunk = "@@ -1,2 +1,3 @@\n context\n-old\n+new added line";
        assert_eq!(last_diff_hunk_line(Some(hunk)), Some("new added line"));
    }

    #[test]
    fn last_diff_hunk_line_strips_the_space_prefix_for_a_context_line() {
        let hunk = "@@ -1,2 +1,2 @@\n context line";
        assert_eq!(last_diff_hunk_line(Some(hunk)), Some("context line"));
    }

    #[test]
    fn last_diff_hunk_line_is_none_for_a_header_only_hunk() {
        assert_eq!(last_diff_hunk_line(Some("@@ -1,2 +1,3 @@")), None);
    }

    #[test]
    fn last_diff_hunk_line_is_none_for_an_absent_or_empty_hunk() {
        assert_eq!(last_diff_hunk_line(None), None);
        assert_eq!(last_diff_hunk_line(Some("")), None);
    }

    #[test]
    fn last_diff_hunk_line_is_none_for_an_unrecognised_prefix() {
        // Never guess: a line with none of `+`/`-`/` ` is treated as
        // unparseable rather than silently used verbatim.
        assert_eq!(
            last_diff_hunk_line(Some("@@ -1 +1 @@\n???not a diff line")),
            None
        );
    }
}
