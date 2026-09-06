//! CT-E7 — review distill: `GET /api/reviews/{id}/distill`.
//!
//! One deterministic JSON dump of a completed review's full LOCAL record
//! — review meta, every patchset, files touched at the latest patchset,
//! the verdict (with `verdict_ps` + staleness), every comment thread
//! (open and resolved, re-resolved against the latest patchset via the
//! same carry-forward ladder [`crate::review_comments`] uses), and every
//! stored suggestion (with its applied-audit trail). Composed STRICTLY
//! from existing local tables/refs — no new storage, no kb call. A
//! review evaporates at GC leaving only commits (see `crate::reviews`'s
//! module doc); this route is how an agent can capture its record BEFORE
//! that happens.
//!
//! # Pure read, deterministic
//!
//! Every input is either immutable (patchset shas/refs, comment anchors)
//! or read fresh at request time (verdict row, suggestion rows, the
//! working blob at each patchset's pinned sha). Re-distilling the same
//! review at the same patchset state yields a byte-identical document
//! apart from `generated_at`.
//!
//! # The kb hand-off is an AGENT-layer decision, never this daemon's
//!
//! kb-code exports data; whether a completed review becomes a kb note or
//! memory is judged by the calling agent, which runs `kb-code review
//! distill <id> --json` and, if it decides the review is worth keeping,
//! authors the artifact itself (`kb notes new` / `kb remember`) citing
//! this review's id + head sha. This route (and its CLI wrapper) never
//! calls kb and never writes anything — ONE call direction stays
//! kb-code→kb, and this route doesn't even use that lane.

use crate::review_comments::build_comment_groups;
use crate::reviews::{files_changed, require_review, resolve_ps, verdict_block};
use crate::routes::ApiError;
use crate::state::SharedState;
use crate::store::{ReviewPatchsetRow, ReviewRow};
use axum::extract::{Path as AxumPath, State};
use axum::http::{header, StatusCode};
use axum::response::IntoResponse;
use axum::Json;
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

pub const SCHEMA: &str = "review-distill/1";

fn now_unix() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/// `GET /api/reviews/{id}/distill` — same gate as the rest of the review
/// READ surface (ordinary `auth_bearer`; loopback bypasses per #4). 404 on
/// an unknown id (`require_review`); 404 "review has no patchsets" on a
/// GC'd-to-zero / never-captured review (the same honest-error convention
/// `resolve_ps`/`review_annotations` already use) — distill has nothing
/// to compose without at least one patchset.
pub async fn review_distill_route(
    State(state): State<SharedState>,
    AxumPath(id): AxumPath<i64>,
) -> Result<impl IntoResponse, ApiError> {
    let (review, repo, _repo_id) = require_review(&state, id).await?;
    let root = repo.path.clone();

    // `repo`'s borrow of `state` ends at the clone above, so `state`
    // itself (an `Arc`) can move into the closure — no extra clone.
    // 2026-08-31 incident (store.rs module doc): `resolve_ps` folded into
    // this EXISTING spawn_blocking (already the sanctioned off-worker
    // path per store.rs's doc) rather than a second `run_blocking` trip —
    // it's a plain `&Store` call, safe on the blocking pool either way.
    let out = tokio::task::spawn_blocking(move || {
        let latest_ps = resolve_ps(&state.store, id, None)?;
        compose_distill(&state, &review, &root, &latest_ps)
    })
    .await
    .map_err(|e| ApiError::new(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))??;

    Ok(([(header::CACHE_CONTROL, "no-store")], Json(out)))
}

fn compose_distill(
    state: &SharedState,
    review: &ReviewRow,
    repo_root: &Path,
    latest_ps: &ReviewPatchsetRow,
) -> Result<serde_json::Value, ApiError> {
    let patchsets: Vec<serde_json::Value> = state
        .store
        .list_patchsets(review.id)?
        .into_iter()
        .map(|ps| {
            serde_json::json!({
                "ps_number": ps.ps_number,
                "tip_sha": ps.tip_sha,
                "base_sha": ps.base_sha,
                "captured_at": ps.captured_at,
            })
        })
        .collect();

    let files = files_changed(repo_root, &latest_ps.base_sha, &latest_ps.tip_sha)?;
    let files_out: Vec<serde_json::Value> = files
        .iter()
        .map(|f| {
            serde_json::json!({
                "path": f.path,
                "old_path": f.old_path,
                "status": f.status,
                "additions": f.insertions,
                "deletions": f.deletions,
            })
        })
        .collect();

    let (verdict, verdict_stale) = verdict_block(review, Some(latest_ps.ps_number));

    // Every thread, open AND resolved — a distilled snapshot, not a live
    // triage queue — re-resolved against the latest patchset.
    let rows = state.store.list_review_annotations(review.id, true)?;
    let parent_meta: Vec<(String, String)> = rows
        .iter()
        .filter(|r| r.parent_id.is_none())
        .map(|r| (r.id.clone(), r.path.clone()))
        .collect();
    let thread_count = parent_meta.len();
    let unresolved_count = rows
        .iter()
        .filter(|r| r.parent_id.is_none() && !r.resolved)
        .count();

    let comments = build_comment_groups(&state.store, repo_root, latest_ps, rows)?;

    // Flat suggestion list with the FULL applied-audit trail
    // (`applied_head_sha`) — the per-comment `suggestion` block above is
    // the read-only wire shape `/comments` already commits to and does
    // not carry it; this is the one place that does.
    let mut suggestions = Vec::with_capacity(parent_meta.len());
    for (annotation_id, path) in &parent_meta {
        if let Some(s) = state.store.get_annotation_suggestion(annotation_id)? {
            suggestions.push(serde_json::json!({
                "annotation_id": annotation_id,
                "path": path,
                "replacement": s.replacement,
                "original": s.original,
                "applied": s.applied,
                "applied_at": s.applied_at,
                "applied_head_sha": s.applied_head_sha,
            }));
        }
    }
    let suggestions_applied_count = suggestions
        .iter()
        .filter(|s| s["applied"].as_bool().unwrap_or(false))
        .count();

    Ok(serde_json::json!({
        "schema": SCHEMA,
        "review": {
            "id": review.id,
            "repo": review.repo,
            "title": review.title,
            "base_ref": review.base_ref,
            "head_ref": review.head_ref,
            "session_id": review.session_id,
            "state": review.state,
            "created_at": review.created_at,
            "updated_at": review.updated_at,
        },
        "patchsets": patchsets,
        "latest_ps": latest_ps.ps_number,
        "files": files_out,
        "verdict": verdict,
        "verdict_stale": verdict_stale,
        "comments": comments,
        "thread_count": thread_count,
        "unresolved_count": unresolved_count,
        "suggestions": suggestions,
        "suggestions_applied_count": suggestions_applied_count,
        "generated_at": now_unix(),
    }))
}
