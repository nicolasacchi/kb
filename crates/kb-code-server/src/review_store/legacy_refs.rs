//! RS-U7 — `kb-code store legacy-refs`/`export-legacy` (README §5.4/§10
//! step 5; D19): cleaning a member clone's now-redundant `refs/kbc/*` once
//! the review store already holds every tip they pinned, and the reverse.
//! MANUAL ONLY — D19's own wording; never wired into boot, seeding, or any
//! scheduled maintenance path in this crate.
//!
//! * `POST /api/repos/{name}/store/legacy-refs {dry_run}` — classify every
//!   `refs/kbc/{pr,review}/*` ref in the member's OWN clone against the
//!   SAME name in the store: `deletable` when the sha matches exactly,
//!   `kept` (with a reason — `sha-differs` or `absent-from-store`)
//!   otherwise. `dry_run=false` deletes the `deletable` set in ONE `git
//!   update-ref --stdin` transaction, IN THE USER CLONE — the second
//!   sanctioned user-clone ref write, beside `checkout::switch_repo`
//!   ([`reviews::delete_refs_transactional`]'s own doc names the first).
//!   Every line is old-value-guarded, so a fetch or checkout racing this
//!   call fails the WHOLE transaction rather than silently deleting a ref
//!   that just moved. `repo_stores.legacy_refs_state` becomes `cleaned`
//!   only when NOTHING `refs/kbc/*`-shaped remains in the clone
//!   afterward — a FULL `refs/kbc/` listing, not this pass's
//!   `Pr`/`Patchset`-only classification — never downgraded, never
//!   written on a dry run.
//! * `POST /api/repos/{name}/store/export-legacy` — the reverse: every
//!   `refs/kbc/{pr,review}/*` THIS repo's own reviews reference (PR
//!   bindings in any state, its own patchsets), written into the clone
//!   CREATE-ONLY ([`reviews::create_only_refs`] — an explicit empty git
//!   old-value asserts "must not already exist"; a ref that appears
//!   between the scan and the write just fails that one line, never the
//!   batch).
//!
//! Both routes require the repo's review store to be READY (`handle_for_
//! repo`) — legacy-refs has nothing to compare against otherwise, and
//! D19's premise is "about a week after the store is stable, once every
//! open review's tip is already reachable in it."

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

use super::registry::{RepoRef, StoreRefusal};
use crate::git::roots::WorkTreeRoot;
use crate::reviews::{self, KbcRef};
use crate::state::SharedState;

pub const LEGACY_REFS_SCHEMA: &str = "kbc-store-legacy-refs/1";
pub const EXPORT_LEGACY_SCHEMA: &str = "kbc-store-export-legacy/1";

fn not_found(name: &str) -> Response {
    (
        StatusCode::NOT_FOUND,
        Json(serde_json::json!({
            "error": format!("unknown repo `{name}`"),
            "type": "urn:kb:errors:not-found",
        })),
    )
        .into_response()
}

fn internal(e: impl std::fmt::Display) -> Response {
    (
        StatusCode::INTERNAL_SERVER_ERROR,
        Json(serde_json::json!({ "error": e.to_string() })),
    )
        .into_response()
}

/// A `legacy_refs`/`export_legacy` computation failure, kept SMALL
/// (`clippy::result_large_err` — `axum::http::Response` itself is well
/// over the 128-byte threshold). `Response` construction happens ONCE, at
/// the route handler that owns the `spawn_blocking` join, never at every
/// fallible step inside the blocking computation.
enum ApplyErr {
    Store(super::registry::StoreUnavailable),
    Internal(String),
}

impl ApplyErr {
    fn into_response(self) -> Response {
        match self {
            ApplyErr::Store(u) => StoreRefusal(u).into_response(),
            ApplyErr::Internal(m) => internal(m),
        }
    }
}

/// One user-clone ref, classified against the store's SAME name (D19).
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct LegacyRefStatus {
    pub kind: &'static str,
    #[serde(rename = "ref")]
    pub refname: String,
    pub sha: String,
    pub deletable: bool,
    /// `"sha-differs"` | `"absent-from-store"` — set iff `!deletable`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<&'static str>,
}

/// Pure classifier (D19: "ONLY when the store holds the SAME ref name at
/// the SAME commit"). No git, no DB — a fixture-testable core.
pub fn classify(
    user_refs: &[(KbcRef, String, String)],
    store_refs: &BTreeMap<String, String>,
) -> Vec<LegacyRefStatus> {
    user_refs
        .iter()
        .map(|(parsed, name, sha)| {
            let (deletable, reason) = match store_refs.get(name) {
                Some(s) if s == sha => (true, None),
                Some(_) => (false, Some("sha-differs")),
                None => (false, Some("absent-from-store")),
            };
            LegacyRefStatus {
                kind: parsed.kind(),
                refname: name.clone(),
                sha: sha.clone(),
                deletable,
                reason,
            }
        })
        .collect()
}

/// `refs/kbc/{pr,review}/*` in the store, as a `name -> sha` map — `Pr`/
/// `Patchset` shapes only (the pre-store grammar never had `-base`/`hint`/
/// `prm`, so those can never be "the same legacy ref"). Store-wide is
/// correct here: `pr/<n>` and `review/<id>/ps<n>` are already globally
/// unique (README §5.1 "members never collide"), so a name match is
/// unambiguous regardless of which member the review belongs to.
fn store_kbc_map(
    git: &super::git::StoreGit,
    git_dir: &std::path::Path,
) -> Result<BTreeMap<String, String>, super::git::StoreGitError> {
    let raw = super::seed::list_refs(git, git_dir, &["refs/kbc/pr/", "refs/kbc/review/"])?;
    Ok(raw
        .into_iter()
        .filter_map(|(oid, name)| match reviews::parse_kbc_ref(&name) {
            Some(KbcRef::Pr { .. }) | Some(KbcRef::Patchset { .. }) => Some((name, oid)),
            _ => None,
        })
        .collect())
}

/// Does the member clone still carry ANY ref under `refs/kbc/`? The FULL
/// prefix listing, deliberately NOT [`reviews::list_kbc_refs`]: that one
/// keeps only the `Pr`/`Patchset` shapes — its own doc names the
/// `ps<n>-base` filter as the reason a `refs/kbc/review/<id>/ps<n>-base`
/// is invisible to it — so it cannot answer "is anything `refs/kbc/*`
/// left?". Read through the store's own allowlisted spawner, exactly as
/// `seed::import_member` reads a member's refs: a `for-each-ref` against
/// the member's common dir, never a write.
fn clone_carries_any_kbc_ref(
    git: &super::git::StoreGit,
    common_dir: &std::path::Path,
) -> Result<bool, super::git::StoreGitError> {
    Ok(!super::seed::list_refs(git, common_dir, &["refs/kbc/"])?.is_empty())
}

fn dry_run_default() -> bool {
    true
}

#[derive(Debug, Deserialize, Default)]
pub struct LegacyRefsBody {
    #[serde(default = "dry_run_default")]
    pub dry_run: bool,
}

/// `POST /api/repos/{name}/store/legacy-refs {dry_run}` — LOOPBACK-ONLY.
pub async fn legacy_refs_route(
    State(state): State<SharedState>,
    Path(name): Path<String>,
    Json(body): Json<LegacyRefsBody>,
) -> Response {
    let Some(repo) = state.review_stores.repo(&name).cloned() else {
        return not_found(&name);
    };
    let st = state.clone();
    let n = name.clone();
    let dry_run = body.dry_run;
    match tokio::task::spawn_blocking(move || legacy_refs_apply(&st, &n, &repo, dry_run)).await {
        Ok(Ok(v)) => Json(v).into_response(),
        Ok(Err(e)) => e.into_response(),
        Err(e) => internal(e),
    }
}

fn legacy_refs_apply(
    state: &SharedState,
    repo_name: &str,
    repo: &RepoRef,
    dry_run: bool,
) -> Result<serde_json::Value, ApplyErr> {
    let handle = state
        .review_stores
        .handle_for_repo(&state.store, repo_name)
        .map_err(ApplyErr::Store)?;
    let git = state
        .review_stores
        .git()
        .ok_or_else(|| ApplyErr::Internal("review store git spawner unavailable".to_string()))?;
    // RS-U5 review fix, applied to this classify-then-mutate path too: the
    // store's `ops` lock is taken BEFORE the store listing and held through
    // the delete, exactly as `reviews::gc_review_refs_inner` takes it for
    // the same sequence. Without it, a capture force-refreshing
    // `refs/kbc/pr/<n>` (`FetchRefspec::new(true, …)`) or a
    // `delete_review`/`gc_patchsets` dropping `refs/kbc/review/<id>/ps<n>`
    // between the scan and the delete leaves the store no longer holding
    // the scanned sha while the clone's pin to it is removed anyway — the
    // commit unreferenced in BOTH repos. The old-value guards in the
    // transaction protect the CLONE's own ref from moving, never the
    // store-side predicate. A blocking acquire is safe and expected here:
    // we are already inside `spawn_blocking` (the same reasoning
    // `gc_review_refs_inner` records).
    let ops = state.review_stores.ops_lock(handle.id);
    let _ops_guard = ops.blocking_lock();
    let store_map =
        store_kbc_map(git, &handle.git_dir).map_err(|e| ApplyErr::Internal(e.to_string()))?;
    let user_refs = reviews::list_kbc_refs(&WorkTreeRoot::user_clone(&repo.root))
        .map_err(|e| ApplyErr::Internal(e.to_string()))?;
    let statuses = classify(&user_refs, &store_map);

    let deletable: Vec<(String, String)> = statuses
        .iter()
        .filter(|s| s.deletable)
        .map(|s| (s.refname.clone(), s.sha.clone()))
        .collect();
    let kept = statuses.len() - deletable.len();

    let mut new_state: Option<&str> = None;
    if !dry_run {
        reviews::delete_refs_transactional(&WorkTreeRoot::user_clone(&repo.root), &deletable)
            .map_err(|e| ApplyErr::Internal(e.to_string()))?;
        // Never downgraded, never rewritten to anything but `cleaned`
        // (the column default is already `present`) — and only when the
        // clone genuinely carries nothing `refs/kbc/*`-shaped anymore.
        // `kept == 0` alone does NOT say that: `statuses` holds only what
        // `reviews::list_kbc_refs` returned, and that lister drops every
        // shape but `Pr`/`Patchset` — a `refs/kbc/review/<id>/ps<n>-base`
        // sits under the very prefix it scans and is filtered out (its own
        // doc names this as the reason the filter exists). The fallback
        // path mints `ps<n>` and `ps<n>-base` from the same `capture_at`,
        // so a clone can reach `cleaned` here with the `-base` pin still in
        // it, contradicting the invariant this module's doc states and
        // leaving a later operator run to report `total: 0` forever. So the
        // claim is decided by a FULL `refs/kbc/` listing instead.
        if kept == 0 {
            let root = &repo.root;
            let common_dir = super::seed::common_dir_of(root)
                .map_err(|e| ApplyErr::Internal(format!("{}: {}", root.display(), e.kind())))?;
            let _ = git.allow_local_source(&common_dir);
            if !clone_carries_any_kbc_ref(git, &common_dir)
                .map_err(|e| ApplyErr::Internal(e.to_string()))?
            {
                new_state = Some("cleaned");
                let _ = state
                    .store
                    .set_repo_store_legacy_refs_state(repo.id, "cleaned");
            }
        }
    }

    Ok(serde_json::json!({
        "schema": LEGACY_REFS_SCHEMA,
        "repo": repo_name,
        "dry_run": dry_run,
        "total": statuses.len(),
        "deletable": deletable.len(),
        "kept": kept,
        "deleted": if dry_run { 0 } else { deletable.len() },
        "refs": statuses,
        "legacy_refs_state": new_state,
    }))
}

/// `POST /api/repos/{name}/store/export-legacy` — LOOPBACK-ONLY. No body.
pub async fn export_legacy_route(
    State(state): State<SharedState>,
    Path(name): Path<String>,
) -> Response {
    let Some(repo) = state.review_stores.repo(&name).cloned() else {
        return not_found(&name);
    };
    let st = state.clone();
    let n = name.clone();
    match tokio::task::spawn_blocking(move || export_legacy_apply(&st, &n, &repo)).await {
        Ok(Ok(v)) => Json(v).into_response(),
        Ok(Err(e)) => e.into_response(),
        Err(e) => internal(e),
    }
}

fn export_legacy_apply(
    state: &SharedState,
    repo_name: &str,
    repo: &RepoRef,
) -> Result<serde_json::Value, ApplyErr> {
    let handle = state
        .review_stores
        .handle_for_repo(&state.store, repo_name)
        .map_err(ApplyErr::Store)?;
    let git = state
        .review_stores
        .git()
        .ok_or_else(|| ApplyErr::Internal("review store git spawner unavailable".to_string()))?;

    // This repo's OWN candidate refs (README §10 step 5): every PR
    // binding in ANY state (export restores history, not just what's
    // currently open) and every one of this member's own patchsets.
    let pr_bound = state
        .store
        .list_pr_bound_reviews(repo_name)
        .map_err(|e| ApplyErr::Internal(e.to_string()))?;
    let patch_keys = state
        .store
        .list_patchset_keys_for_repo(repo_name)
        .map_err(|e| ApplyErr::Internal(e.to_string()))?;
    let mut want: Vec<String> = Vec::new();
    for (_, pr_number, _) in &pr_bound {
        if (1..=i64::from(u32::MAX)).contains(pr_number) {
            want.push(reviews::pr_ref(*pr_number as u32));
        }
    }
    for (review_id, ps_number) in &patch_keys {
        want.push(reviews::patchset_ref(*review_id, *ps_number));
    }
    want.sort();
    want.dedup();

    let store_map =
        store_kbc_map(git, &handle.git_dir).map_err(|e| ApplyErr::Internal(e.to_string()))?;
    let candidates: Vec<(String, String)> = want
        .into_iter()
        .filter_map(|name| store_map.get(&name).cloned().map(|sha| (name, sha)))
        .collect();

    let created = reviews::create_only_refs(&WorkTreeRoot::user_clone(&repo.root), &candidates);
    Ok(serde_json::json!({
        "schema": EXPORT_LEGACY_SCHEMA,
        "repo": repo_name,
        "candidates": candidates.len(),
        "created": created.len(),
        "refs": created,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sha(b: u8) -> String {
        format!("{b:02x}").repeat(20)
    }

    fn kbc(name: &str) -> (KbcRef, String, String) {
        (
            reviews::parse_kbc_ref(name).unwrap(),
            name.to_string(),
            sha(1),
        )
    }

    #[test]
    fn same_name_same_sha_is_deletable() {
        let user = vec![kbc("refs/kbc/pr/42")];
        let mut store = BTreeMap::new();
        store.insert("refs/kbc/pr/42".to_string(), sha(1));
        let out = classify(&user, &store);
        assert_eq!(out.len(), 1);
        assert!(out[0].deletable, "{out:?}");
        assert_eq!(out[0].reason, None);
    }

    #[test]
    fn differing_sha_is_kept_and_reported() {
        let user = vec![kbc("refs/kbc/pr/42")];
        let mut store = BTreeMap::new();
        store.insert("refs/kbc/pr/42".to_string(), sha(2));
        let out = classify(&user, &store);
        assert!(!out[0].deletable);
        assert_eq!(out[0].reason, Some("sha-differs"));
    }

    #[test]
    fn absent_from_store_is_kept_and_reported() {
        let user = vec![kbc("refs/kbc/review/7/ps1")];
        let store = BTreeMap::new();
        let out = classify(&user, &store);
        assert!(!out[0].deletable);
        assert_eq!(out[0].reason, Some("absent-from-store"));
    }
}
