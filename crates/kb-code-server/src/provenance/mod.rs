//! W3.3 + W3.4 — kb-code's session-PROVENANCE surface, built entirely on top
//! of W3.1's blame service (`crate::blame`) and W3.2's join ladder
//! (`crate::join::ladder`): neither is reimplemented or bypassed here, only
//! composed.
//!
//! - [`why`] — line-grade ("which session produced THIS line") and
//!   file-grade ("which sessions dominate this file") attribution
//!   (`GET /api/why`, `kb-code why`).
//! - [`story`] — a file's (or one symbol's) session TIMELINE, current
//!   owners plus superseded drive-by touches (`GET /api/story`,
//!   `kb-code story`).
//! - [`report`] — the W3.3 INSTRUMENT: join-ladder confidence/via/
//!   trailer-coverage counts across a repo's commit history
//!   (`GET /api/provenance-report`, `kb-code provenance-report`) —
//!   supersedes kb-cli's W0.6 probe (`kb sessions provenance-report`, left
//!   untouched: that one measures against kb's raw commit-map feed as a
//!   pre-Wave-3 yardstick, this one runs the REAL 6-arm ladder per commit).
//!
//! # Shared plumbing
//!
//! [`blame_regions`] and [`regions_with_attribution`] are the one on-ramp
//! from "a file, at its CURRENT state" to "every line's commit AND (when
//! resolvable) session" — both `why`'s file-grade query and `story`'s
//! current-owners pass go through them, so a sha appearing in several
//! regions of the same file is resolved through the ladder EXACTLY ONCE per
//! request (an in-process `HashMap`, layered on top of the ladder's own
//! `commit_sessions` sqlite cache — see [`resolve_sha_cached`]).
//!
//! # Reading the transcripts store from `why`
//!
//! [`UNCOMMITTED_SHA`] lines (git's own "not yet committed" blame sentinel)
//! have no commit to join through at all. `why`'s line-grade query answers
//! them instead from the LOCAL W2.5 transcripts index
//! (`store::Store::transcript_sessions_touching_path`) — which in-flight
//! session(s) most recently touched this exact path. This is a deliberate,
//! narrow exception to that lane's "never in any non-loopback response"
//! posture (`transcripts`' own module doc): only opaque SESSION IDS cross
//! the wire here, never a snippet of transcript text, and a bare session id
//! is already the same information class `GET /api/join/commit` exposes
//! over the ordinary `auth_bearer` gate for every resolved commit — so
//! `/api/why`/`/api/story` are mounted on that SAME ordinary gate
//! (`router.rs`), not the transcripts lane's stricter `loopback_only`.

pub mod report;
pub mod story;
pub mod why;

use crate::blame::{self, BlameRegion};
use crate::config::RepoEntry;
use crate::git::GitRepo;
use crate::join::ladder::{self, Attribution};
use crate::routes::ApiError;
use crate::state::SharedState;
use axum::http::StatusCode;
use std::collections::HashMap;

/// git's own sentinel sha for an uncommitted (dirty-blame) line — 40 ASCII
/// zeroes, `git blame --contents`'s "not yet committed" marker (see
/// `blame`'s own module doc, "clean vs dirty"). Not exported from `blame`
/// itself (that module's own tests spell it `"0".repeat(40)` ad hoc); named
/// here since `why`/`story` both need to recognise it explicitly.
pub const UNCOMMITTED_SHA: &str = "0000000000000000000000000000000000000000";

/// Blame `path` at the repo's CURRENT state (mirrors `routes::blame`'s
/// `rev=None` shape) and return the resulting regions, optionally narrowed
/// to `line_range` (same "whole overlapping groups, never cropped"
/// contract as `GET /api/blame`'s own `start`/`end`). The blocking
/// subprocess work runs in `spawn_blocking`, reopening `GitRepo` fresh
/// there — see `routes::blame`'s own doc for why (`GitRepo` is `!Send`).
pub(crate) async fn blame_regions(
    state: &SharedState,
    repo: &RepoEntry,
    repo_id: i64,
    path: &str,
    line_range: Option<(u32, u32)>,
) -> Result<Vec<BlameRegion>, ApiError> {
    let repo_root = repo.path.clone();
    let blame_cache = state.blame_cache.clone();
    let path_owned = path.to_string();
    let result = tokio::task::spawn_blocking(move || -> blame::Result<blame::BlameResult> {
        let git = GitRepo::open(&repo_root)?;
        blame::blame_file(
            &blame_cache,
            &git,
            repo_id,
            &repo_root,
            &path_owned,
            None,
            line_range,
        )
    })
    .await
    .map_err(|e| {
        ApiError::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("blame task panicked: {e}"),
        )
    })??;
    Ok(result.regions)
}

/// The bounded, on-demand line-history call (`blame::line_timeline`,
/// `git log -L`), wrapped the same `spawn_blocking` way as
/// [`blame_regions`] — `story`'s drive-by/historical pass samples this once
/// per current blame region.
pub(crate) async fn line_timeline(
    state: &SharedState,
    repo: &RepoEntry,
    path: &str,
    line: u32,
) -> Result<Vec<blame::TimelineEntry>, ApiError> {
    let _ = state; // symmetry with `blame_regions`'s signature; no daemon state needed today.
    let repo_root = repo.path.clone();
    let path_owned = path.to_string();
    tokio::task::spawn_blocking(move || {
        blame::line_timeline(&repo_root, &path_owned, line, blame::DEFAULT_MAX_ENTRIES)
    })
    .await
    .map_err(|e| {
        ApiError::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("timeline task panicked: {e}"),
        )
    })?
    .map_err(crate::routes::timeline_to_api_error)
}

/// Read-through a per-request `HashMap` cache before calling
/// `ladder::resolve_commit` — the SAME sha resolved twice in one
/// `why`/`story` request (a common shape: an old, rarely-touched region
/// repeated across a file, or a region's owning sha reappearing in another
/// region's bounded timeline) is only ever ladder-resolved once per
/// request, on top of whatever the ladder's own `commit_sessions` cache
/// already saves across REQUESTS.
pub(crate) async fn resolve_sha_cached(
    repo: &RepoEntry,
    repo_id: i64,
    sha: &str,
    state: &SharedState,
    cache: &mut HashMap<String, Attribution>,
) -> Attribution {
    if let Some(a) = cache.get(sha) {
        return a.clone();
    }
    let a = ladder::resolve_commit(repo, repo_id, sha, &state.store, &state.kb_client).await;
    cache.insert(sha.to_string(), a.clone());
    a
}

/// Blame `path`'s CURRENT state, then resolve every region's sha through
/// the join ladder — `why`'s file-grade query's data source. A region whose
/// sha is [`UNCOMMITTED_SHA`] carries `None` (there is no commit to
/// resolve); every other region carries `Some(Attribution)`, deduped via
/// [`resolve_sha_cached`].
pub async fn regions_with_attribution(
    state: &SharedState,
    repo: &RepoEntry,
    repo_id: i64,
    path: &str,
) -> Result<Vec<(BlameRegion, Option<Attribution>)>, ApiError> {
    let regions = blame_regions(state, repo, repo_id, path, None).await?;
    let mut cache = HashMap::new();
    let mut out = Vec::with_capacity(regions.len());
    for region in regions {
        if region.sha == UNCOMMITTED_SHA {
            out.push((region, None));
            continue;
        }
        let attribution = resolve_sha_cached(repo, repo_id, &region.sha, state, &mut cache).await;
        out.push((region, Some(attribution)));
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn uncommitted_sha_is_forty_ascii_zeroes() {
        assert_eq!(UNCOMMITTED_SHA.len(), 40);
        assert!(UNCOMMITTED_SHA.bytes().all(|b| b == b'0'));
        assert_eq!(UNCOMMITTED_SHA, "0".repeat(40));
    }
}
