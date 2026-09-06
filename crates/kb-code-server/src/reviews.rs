//! V3.R1 ("Review cockpit") — local Gerrit-lite review sessions.
//!
//! A review is a named `(base_ref, head_ref)` pair with ordered patchset
//! snapshots under `refs/kbc/review/<id>/ps<n>`, viewed-file tracking
//! (Reviewable-style `blob_sha` staleness), and open-annotation counts on
//! the change set. Single operator; no approvals, merge, or notifications.
//!
//! # Mutation surface (git)
//!
//! The ONLY refs this module writes are under `refs/kbc/review/<id>/ps<n>`:
//!
//! - `git rev-parse --verify <spec>^{commit}` — resolve tip / validate sha
//! - `git merge-base <base_ref> <tip>` — base_sha at capture
//! - `git update-ref refs/kbc/review/<id>/ps<n> <sha>` — pin a patchset
//! - `git update-ref -d refs/kbc/review/<id>/ps<n>` — GC / delete
//! - read-only: `diff --name-status/-numstat`, `rev-list --count`,
//!   `ls-tree`, `range-diff` (via `history::` helpers)
//!
//! `id` and `n` are daemon-generated integers; shas are validated as
//! full 40-hex after `rev-parse` before any `update-ref`. User-supplied
//! ref names go through [`reject_user_ref`] (no leading `-`, no
//! whitespace/NUL/control chars) before any argv position.
//!
//! # Routes
//!
//! **Loopback-only** (beside `/api/checkout` / `/api/prs/fetch`):
//! `POST /api/reviews`, `POST /api/reviews/{id}/snapshot`,
//! `PATCH /api/reviews/{id}`, `DELETE /api/reviews/{id}`,
//! `PUT /api/reviews/{id}/viewed`, `DELETE /api/reviews/{id}/viewed/{path}`,
//! `POST /api/reviews/pr` ([`create_review_pr`], PRR-R2),
//! `PUT /api/reviews/{id}/report` ([`put_review_report`], PRR-R2).
//!
//! **Gated** (S2-B, `router.rs`'s `review_remote` sub-router — loopback
//! unconditionally, else `[review] remote_mutations`-gated bearer, default
//! OFF ⇒ 404 for a non-loopback caller, byte-identical to the loopback-only
//! posture above): `PUT`/`DELETE /api/reviews/{id}/verdict` (V4.C2). See
//! [`crate::review_gate::review_mutations_gate`] for the full admission
//! table this route (and four sibling routes in `crate::review_findings` /
//! `crate::review_github_export`) now shares.
//!
//! **Bearer** (reads): `GET /api/reviews`, `GET /api/reviews/{id}`,
//! `GET /api/reviews/{id}/files`, `GET /api/reviews/{id}/interdiff`,
//! `GET /api/reviews/{id}/annotations`, `GET /api/reviews/{id}/comments`
//! ([`crate::review_comments`]), `GET /api/reviews/{id}/risk`,
//! `GET /api/reviews/{id}/map`, `GET /api/reviews/{id}/reading-order`
//! (map + reading-order live in [`crate::review_map`]), `GET
//! /api/reviews/{id}/distill` (CT-E7, [`crate::review_distill`]) — one
//! deterministic JSON dump of the review's full local record for the
//! AGENT layer to journal (kb-code itself never writes to kb). PRR-R2 adds
//! `GET /api/reviews/{id}/report` ([`get_review_report`]) and
//! `GET /api/reviews/{id}/artifact` ([`get_review_artifact`], the live,
//! unpersisted kb doc-hint verification — the ONE kb-code->kb call this
//! unit adds, via `join::kb_client::KbClient::doc_meta`). PRR-R4 adds
//! `GET /api/reviews/{id}/pr-status` ([`pr_status_route`], design doc §2
//! row 12 — the staleness probe) to this same bearer surface, and widens
//! `GET /api/reviews`/`GET /api/reviews/{id}`'s own response shape with the
//! R2-leftover additive PR-binding/report-summary fields (see
//! `pr_binding_and_report_fields`'s doc). `GET /api/reviews/inbox`
//! ([`crate::review_inbox::list_inbox_route`]) and
//! `GET /api/reviews/{id}/timeline`
//! ([`crate::review_timeline::review_timeline_route`]) are their own new
//! sibling modules, wired on the same bearer router in `router.rs`.

use crate::config::RepoEntry;
use crate::git::{GitRepo, Revspec};
use crate::history::{self, HistoryError};
use crate::routes::{find_repo, safe_rel_path, ApiError};
use crate::state::SharedState;
use crate::store::{
    ReviewPatchsetRow, ReviewPrBinding, ReviewReport, ReviewRow, Store, StoreBlocking,
};
use axum::extract::{Path as AxumPath, Query, State};
use axum::http::{header, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::Json;
use kb_core::events::EventBus;
use serde::Deserialize;
use std::collections::HashMap;
use std::path::Path;
use std::process::Command;
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

pub const SCHEMA: &str = "reviews/1";

/// Auto-capture debounce — a rebase moves the tip many times; capture
/// once when it settles for at least this long.
pub const AUTO_CAPTURE_DEBOUNCE: Duration = Duration::from_secs(5);

// --- pure decision (unit-tested) -------------------------------------------

/// Pure "should we capture a patchset now?" decision. Unit-tested so the
/// debounce / close / tip-equals / config gates stay honest without
/// spinning a daemon.
///
/// - `patchset_capture_enabled` — `[review] patchset_capture` master switch
///   (explicit `POST …/snapshot` ignores this; only auto-capture consults it)
/// - `review_is_open` — closed reviews never auto-capture
/// - `tip_equals_latest_ps` — skip when head_ref tip already matches the
///   latest patchset's `tip_sha`
/// - `debounce_settled` — tip has been stable for ≥ [`AUTO_CAPTURE_DEBOUNCE`]
pub fn should_auto_capture(
    patchset_capture_enabled: bool,
    review_is_open: bool,
    tip_equals_latest_ps: bool,
    debounce_settled: bool,
) -> bool {
    patchset_capture_enabled && review_is_open && !tip_equals_latest_ps && debounce_settled
}

// --- git guards + ref ops --------------------------------------------------

#[derive(Debug, thiserror::Error)]
pub enum ReviewGitError {
    #[error("failed to spawn git: {0}")]
    Spawn(std::io::Error),
    #[error("git failed (exit {status}): {stderr}")]
    GitFailed { status: i32, stderr: String },
    #[error("invalid ref name: {0:?}")]
    BadRef(String),
    #[error("invalid commit sha: {0:?}")]
    BadSha(String),
    #[error("could not resolve {0:?}")]
    Unresolved(String),
    #[error("no merge-base between {0:?} and {1:?}")]
    NoMergeBase(String, String),
}

impl From<HistoryError> for ReviewGitError {
    fn from(e: HistoryError) -> Self {
        match e {
            HistoryError::Spawn(io) => ReviewGitError::Spawn(io),
            HistoryError::GitFailed { status, stderr } => {
                ReviewGitError::GitFailed { status, stderr }
            }
            HistoryError::BadRevspec(s) => ReviewGitError::BadRef(s),
            HistoryError::NotFound(s) => ReviewGitError::Unresolved(s),
        }
    }
}

impl From<ReviewGitError> for ApiError {
    fn from(e: ReviewGitError) -> Self {
        match e {
            ReviewGitError::Spawn(_) => {
                ApiError::new(StatusCode::INTERNAL_SERVER_ERROR, e.to_string())
            }
            ReviewGitError::GitFailed { .. } => ApiError::bad_request(e.to_string()),
            ReviewGitError::BadRef(_) | ReviewGitError::BadSha(_) => {
                ApiError::bad_request(e.to_string())
            }
            ReviewGitError::Unresolved(s) => {
                ApiError::not_found(format!("could not resolve {s:?}"))
            }
            ReviewGitError::NoMergeBase(_, _) => ApiError::bad_request(e.to_string()),
        }
    }
}

/// Reject a caller-supplied ref name before it ever reaches argv.
///
/// Mirrors `history::reject_dash_prefixed` (no leading `-` so git never
/// treats it as a flag) and additionally forbids whitespace / NUL /
/// control characters and empty strings. Does NOT validate that the ref
/// exists — that's `resolve_commit_sha`'s job.
pub fn reject_user_ref(name: &str) -> Result<(), ReviewGitError> {
    parse_user_ref(name).map(|_| ())
}

/// V70-A2 (SEC-17) — the TYPED form of [`reject_user_ref`]: the same
/// predicate, but it hands back a [`Revspec`] the caller can only have
/// obtained by passing it. New call sites take this; `reject_user_ref`
/// survives as the thin `()`-returning shim for the sites that only need
/// the gate, and delegates here so the two can never drift.
pub fn parse_user_ref(name: &str) -> Result<Revspec, ReviewGitError> {
    Revspec::parse(name).map_err(|e| ReviewGitError::BadRef(e.0))
}

/// A full 40-char lowercase hex object id — the ONLY shape we ever pass
/// to `git update-ref` as the new tip.
pub fn is_full_sha(s: &str) -> bool {
    s.len() == 40 && s.chars().all(|c| c.is_ascii_hexdigit())
}

/// `refs/kbc/review/<id>/ps<n>` — built only from daemon-generated integers.
pub fn patchset_ref(review_id: i64, ps_number: i64) -> String {
    format!("refs/kbc/review/{review_id}/ps{ps_number}")
}

fn run_git(repo_root: &Path, args: &[&str]) -> Result<Vec<u8>, ReviewGitError> {
    let output = Command::new("git")
        .arg("-C")
        .arg(repo_root)
        .args(args)
        .output()
        .map_err(ReviewGitError::Spawn)?;
    if !output.status.success() {
        return Err(ReviewGitError::GitFailed {
            status: output.status.code().unwrap_or(-1),
            stderr: String::from_utf8_lossy(&output.stderr).trim().to_string(),
        });
    }
    Ok(output.stdout)
}

/// `git rev-parse --verify <spec>^{commit}` → full 40-hex sha.
pub fn resolve_commit_sha(repo_root: &Path, spec: &Revspec) -> Result<String, ReviewGitError> {
    // V70-A2 (SEC-17) — the `reject_user_ref(spec)?` line that used to
    // open this fn is now the CONSTRUCTOR of the type it takes: there is
    // no longer a guard a caller (or this fn) can forget, and the
    // `{spec}^{{commit}}` interpolation below can only ever be fed a
    // validated revspec.
    let spec = spec.as_str();
    let out = run_git(
        repo_root,
        &["rev-parse", "--verify", &format!("{spec}^{{commit}}")],
    )
    .map_err(|e| match e {
        ReviewGitError::GitFailed { .. } => ReviewGitError::Unresolved(spec.to_string()),
        other => other,
    })?;
    let sha = String::from_utf8_lossy(&out).trim().to_string();
    if !is_full_sha(&sha) {
        return Err(ReviewGitError::BadSha(sha));
    }
    Ok(sha)
}

/// `git merge-base <a> <b>` — both sides must already be full shas.
pub fn merge_base_sha(
    repo_root: &Path,
    base_sha: &str,
    tip_sha: &str,
) -> Result<String, ReviewGitError> {
    if !is_full_sha(base_sha) {
        return Err(ReviewGitError::BadSha(base_sha.to_string()));
    }
    if !is_full_sha(tip_sha) {
        return Err(ReviewGitError::BadSha(tip_sha.to_string()));
    }
    let out = run_git(repo_root, &["merge-base", base_sha, tip_sha]).map_err(|e| match e {
        ReviewGitError::GitFailed { .. } => {
            ReviewGitError::NoMergeBase(base_sha.to_string(), tip_sha.to_string())
        }
        other => other,
    })?;
    let sha = String::from_utf8_lossy(&out).trim().to_string();
    if !is_full_sha(&sha) {
        return Err(ReviewGitError::BadSha(sha));
    }
    Ok(sha)
}

/// `git update-ref refs/kbc/review/<id>/ps<n> <sha>`.
///
/// `review_id` / `ps_number` are daemon integers (Display → digits only);
/// `sha` must be a full 40-hex string (validated BEFORE spawn). Never
/// passes user text into the ref path or the sha position.
pub fn update_patchset_ref(
    repo_root: &Path,
    review_id: i64,
    ps_number: i64,
    sha: &str,
) -> Result<(), ReviewGitError> {
    if !is_full_sha(sha) {
        return Err(ReviewGitError::BadSha(sha.to_string()));
    }
    if review_id < 1 || ps_number < 1 {
        return Err(ReviewGitError::BadRef(format!(
            "review_id={review_id} ps_number={ps_number}"
        )));
    }
    // Re-verify the sha resolves as a commit in THIS repo before writing
    // the ref (injection-safe: sha is already full-hex).
    let verified = run_git(
        repo_root,
        &["rev-parse", "--verify", &format!("{sha}^{{commit}}")],
    )?;
    let verified = String::from_utf8_lossy(&verified).trim().to_string();
    if verified != sha {
        return Err(ReviewGitError::BadSha(sha.to_string()));
    }
    let refname = patchset_ref(review_id, ps_number);
    // argv: update-ref <refname> <sha> — both positions shape-validated.
    run_git(repo_root, &["update-ref", &refname, sha])?;
    Ok(())
}

/// `git update-ref -d refs/kbc/review/<id>/ps<n>`.
pub fn delete_patchset_ref(
    repo_root: &Path,
    review_id: i64,
    ps_number: i64,
) -> Result<(), ReviewGitError> {
    if review_id < 1 || ps_number < 1 {
        return Err(ReviewGitError::BadRef(format!(
            "review_id={review_id} ps_number={ps_number}"
        )));
    }
    let refname = patchset_ref(review_id, ps_number);
    // -d is a fixed flag we control; refname is digits-only.
    let output = Command::new("git")
        .arg("-C")
        .arg(repo_root)
        .args(["update-ref", "-d", &refname])
        .output()
        .map_err(ReviewGitError::Spawn)?;
    // Missing ref is fine (already GC'd / never written) — treat non-zero
    // as soft when stderr mentions "unable to resolve" / "no such ref".
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr).to_lowercase();
        if stderr.contains("unable to resolve")
            || stderr.contains("no such")
            || stderr.contains("cannot lock ref")
            || stderr.contains("doesn't exist")
        {
            return Ok(());
        }
        return Err(ReviewGitError::GitFailed {
            status: output.status.code().unwrap_or(-1),
            stderr: String::from_utf8_lossy(&output.stderr).trim().to_string(),
        });
    }
    Ok(())
}

/// `git rev-list --count <base>..<tip>` — both full shas.
pub fn commit_count(
    repo_root: &Path,
    base_sha: &str,
    tip_sha: &str,
) -> Result<u64, ReviewGitError> {
    if !is_full_sha(base_sha) || !is_full_sha(tip_sha) {
        return Err(ReviewGitError::BadSha(format!("{base_sha}..{tip_sha}")));
    }
    let revspec = format!("{base_sha}..{tip_sha}");
    let out = run_git(repo_root, &["rev-list", "--count", &revspec])?;
    let s = String::from_utf8_lossy(&out).trim().to_string();
    s.parse().map_err(|_| ReviewGitError::GitFailed {
        status: -1,
        stderr: format!("malformed rev-list --count output: {s:?}"),
    })
}

/// Blob oid of `path` at `tip_sha`, or empty string if the path is absent
/// (deleted file). Uses `git ls-tree <tip> -- <path>` so the path never
/// sits in a revspec position.
pub fn blob_sha_at(repo_root: &Path, tip_sha: &str, path: &str) -> Result<String, ReviewGitError> {
    if !is_full_sha(tip_sha) {
        return Err(ReviewGitError::BadSha(tip_sha.to_string()));
    }
    // Path is a pathspec after `--`; still reject traversal / control.
    safe_rel_path(path).map_err(|_| ReviewGitError::BadRef(path.to_string()))?;
    let out = run_git(repo_root, &["ls-tree", tip_sha, "--", path])?;
    let text = String::from_utf8_lossy(&out);
    // `<mode> <type> <sha>\t<path>`
    let line = text.lines().next().unwrap_or("");
    if line.is_empty() {
        return Ok(String::new());
    }
    let meta = line.split('\t').next().unwrap_or("");
    let sha = meta.split_whitespace().nth(2).unwrap_or("");
    if sha.len() == 40 && sha.chars().all(|c| c.is_ascii_hexdigit()) {
        Ok(sha.to_string())
    } else {
        Ok(String::new())
    }
}

/// Diff files between two shas (`base_sha..tip_sha`) via `history::diff_files`.
pub fn files_changed(
    repo_root: &Path,
    base_sha: &str,
    tip_sha: &str,
) -> Result<Vec<crate::numstat::FileChange>, ReviewGitError> {
    if !is_full_sha(base_sha) || !is_full_sha(tip_sha) {
        return Err(ReviewGitError::BadSha(format!("{base_sha}..{tip_sha}")));
    }
    let range = format!("{base_sha}..{tip_sha}");
    history::diff_files(repo_root, "diff", &["-M", &range]).map_err(Into::into)
}

/// Default branch name — same signal `/api/branches` uses
/// ([`crate::git::default_branch`]: `refs/remotes/origin/HEAD`, else
/// `GitRepo::head_info().branch`). Falls back to `"main"` when both
/// miss (detached/unborn, no origin HEAD).
pub fn default_base_ref(repo_root: &Path) -> String {
    match GitRepo::open(repo_root) {
        Ok(git) => crate::git::default_branch(&git).unwrap_or_else(|| "main".to_string()),
        Err(_) => "main".to_string(),
    }
}

fn now_unix() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

fn short_sha(sha: &str) -> String {
    sha.chars().take(12).collect()
}

/// V4.C2 — every `review.changed` emission carries an additive `reason`
/// (`patchset` | `meta` | `deleted` | `verdict` | `pr_bound` |
/// `pr_refreshed` — PRR-R8 adds the last one). Existing consumers ignore
/// unknown fields. `pub(crate)` (PRR-R8) — `review_sweep` reuses this
/// rather than a second `review.changed` composition, so the event shape
/// can never drift between the two emitters.
pub(crate) fn emit_review_changed(
    bus: &EventBus,
    review_id: i64,
    repo: &str,
    reason: &str,
    deleted: bool,
) {
    let mut body = serde_json::json!({
        "review_id": review_id,
        "repo": repo,
        "reason": reason,
    });
    if deleted {
        body["deleted"] = serde_json::json!(true);
    }
    bus.emit("review.changed", body);
}

/// Wire verdict block + `verdict_stale`. Stale iff a verdict is set
/// AND `verdict_ps < latest_ps`. False when there is no verdict or no
/// newer patchset.
///
/// `pub(crate)` — CT-E7's `review_distill` reuses this so the distill
/// document's verdict block is byte-identical to `get_review`'s.
pub(crate) fn verdict_block(
    review: &ReviewRow,
    latest_ps: Option<i64>,
) -> (serde_json::Value, bool) {
    let Some(state) = review.verdict.as_deref() else {
        return (serde_json::Value::Null, false);
    };
    let stale = match (review.verdict_ps, latest_ps) {
        (Some(vp), Some(lp)) => vp < lp,
        _ => false,
    };
    (
        serde_json::json!({
            "state": state,
            "note": review.verdict_note,
            "at": review.verdict_at,
            "ps": review.verdict_ps,
        }),
        stale,
    )
}

/// PRR-R4 — the R2-leftover additive fields every review READ surface
/// (`GET /api/reviews`, `GET /api/reviews/{id}`) gains: the PR binding
/// (parsed `pr_meta`, not the raw JSON string), the artifact hint pair
/// (flat `artifact_hint_kb`/`artifact_hint_id`, matching `patch_review`'s
/// own response shape rather than a nested object), and a cheap report
/// summary (`has_report` + `report_risk_score`, the LATTER a bare parse of
/// the stored `report_json` blob's own `risk_score` key — never a
/// re-computation of `GET /reviews/{id}/risk`'s live composite, and never a
/// daemon-authored verdict of its own: whatever value is there is exactly
/// what the report's author already wrote). Every key here is NEW; nothing
/// existing is touched, so a pre-R4 consumer parsing this JSON is
/// unaffected by these fields' presence.
fn pr_binding_and_report_fields(
    binding: &ReviewPrBinding,
    report: &ReviewReport,
) -> serde_json::Value {
    let pr_meta: Option<serde_json::Value> = binding
        .pr_meta_json
        .as_deref()
        .and_then(|s| serde_json::from_str(s).ok());
    let has_report = report.report_json.is_some();
    let report_risk_score = report
        .report_json
        .as_deref()
        .and_then(|s| serde_json::from_str::<serde_json::Value>(s).ok())
        .and_then(|v| v.get("risk_score").cloned())
        .unwrap_or(serde_json::Value::Null);
    serde_json::json!({
        "pr_number": binding.pr_number,
        "pr_repo_slug": binding.pr_repo_slug,
        "pr_head_sha": binding.pr_head_sha,
        "pr_meta": pr_meta,
        "pr_meta_fetched_at": binding.pr_meta_fetched_at,
        "artifact_hint_kb": binding.artifact_hint_kb,
        "artifact_hint_id": binding.artifact_hint_id,
        "has_report": has_report,
        "report_risk_score": report_risk_score,
    })
}

/// Splice [`pr_binding_and_report_fields`]'s keys into an existing review
/// JSON object in place — `list_reviews`/`get_review` build their base
/// shape via the `json!` macro (a `serde_json::Value::Object`), so this
/// just extends that map rather than requiring every call site to
/// hand-merge two objects.
fn merge_pr_binding_and_report_fields(
    value: &mut serde_json::Value,
    binding: &ReviewPrBinding,
    report: &ReviewReport,
) {
    if let (serde_json::Value::Object(dst), serde_json::Value::Object(src)) =
        (value, pr_binding_and_report_fields(binding, report))
    {
        dst.extend(src);
    }
}

const VERDICT_STATES: &[&str] = &["comment", "approve", "request-changes"];

/// `pub(crate)` (V70-R) so [`crate::review_findings::compose_review_route`]
/// can validate a `compose` body's optional review-level verdict with the
/// SAME rule [`put_verdict`] enforces, rather than a second copy of
/// [`VERDICT_STATES`].
pub(crate) fn parse_verdict_state(s: &str) -> Result<(), ApiError> {
    if VERDICT_STATES.contains(&s) {
        Ok(())
    } else {
        Err(ApiError::bad_request(format!(
            "verdict state must be comment|approve|request-changes, got {s:?}"
        )))
    }
}

// --- capture core ----------------------------------------------------------

/// Capture a new patchset for `review_id`. Returns the new `ps_number`.
///
/// Steps: resolve tip of `head_ref` → skip if equals latest tip (when
/// `skip_if_same`) → merge-base → GC oldest if over max → update-ref →
/// insert row → emit `review.changed`.
pub fn capture_patchset(
    store: &Store,
    bus: &EventBus,
    repo_root: &Path,
    review: &ReviewRow,
    max_patchsets: u32,
    skip_if_same: bool,
) -> Result<ReviewPatchsetRow, ReviewGitError> {
    let tip_sha = resolve_commit_sha(repo_root, &parse_user_ref(&review.head_ref)?)?;
    if skip_if_same {
        if let Ok(Some(latest)) = store.latest_patchset(review.id) {
            if latest.tip_sha == tip_sha {
                return Ok(latest);
            }
        }
    }
    // Resolve base_ref for merge-base (may be a branch name).
    let base_resolved = resolve_commit_sha(repo_root, &parse_user_ref(&review.base_ref)?)?;
    let base_sha = merge_base_sha(repo_root, &base_resolved, &tip_sha)?;

    // Reserve the next ps_number BEFORE GC so numbers stay monotonic even
    // when every retained patchset is deleted to make room (max=1 + count=2
    // would otherwise re-mint ps1 after MAX collapses to NULL).
    let ps_number = store
        .next_ps_number(review.id)
        .map_err(|e| ReviewGitError::GitFailed {
            status: -1,
            stderr: e.to_string(),
        })?;

    // GC oldest while at/over capacity so the insert below lands within
    // max_patchsets (ps_number itself is already reserved above).
    while store.patchset_count(review.id).unwrap_or(0) as u32 >= max_patchsets.max(1) {
        if let Ok(Some(old)) = store.oldest_patchset(review.id) {
            let _ = delete_patchset_ref(repo_root, review.id, old.ps_number);
            let _ = store.delete_patchset(review.id, old.ps_number);
        } else {
            break;
        }
    }

    update_patchset_ref(repo_root, review.id, ps_number, &tip_sha)?;
    let captured_at = now_unix();
    store
        .insert_patchset(review.id, ps_number, &tip_sha, &base_sha, captured_at)
        .map_err(|e| ReviewGitError::GitFailed {
            status: -1,
            stderr: e.to_string(),
        })?;

    emit_review_changed(bus, review.id, &review.repo, "patchset", false);

    store
        .get_patchset(review.id, ps_number)
        .map_err(|e| ReviewGitError::GitFailed {
            status: -1,
            stderr: e.to_string(),
        })?
        .ok_or_else(|| ReviewGitError::GitFailed {
            status: -1,
            stderr: "patchset row missing after insert".into(),
        })
}

/// Delete every patchset ref for a review (best-effort), then the row.
pub fn delete_review_with_refs(
    store: &Store,
    bus: &EventBus,
    repo_root: &Path,
    review: &ReviewRow,
) -> Result<(), ReviewGitError> {
    let pss = store.list_patchsets(review.id).unwrap_or_default();
    for ps in &pss {
        let _ = delete_patchset_ref(repo_root, review.id, ps.ps_number);
    }
    store
        .delete_review(review.id)
        .map_err(|e| ReviewGitError::GitFailed {
            status: -1,
            stderr: e.to_string(),
        })?;
    emit_review_changed(bus, review.id, &review.repo, "deleted", true);
    Ok(())
}

/// GC oldest patchsets for one review (or every review when `review_id`
/// is `None`) down to `max_patchsets`. Used by `kb-code review gc` and
/// the capture path.
pub fn gc_patchsets(
    store: &Store,
    repos: &[RepoEntry],
    review_id: Option<i64>,
    max_patchsets: u32,
) -> Result<u32, ReviewGitError> {
    let max = max_patchsets.max(1);
    let mut deleted = 0u32;
    let reviews: Vec<ReviewRow> = if let Some(id) = review_id {
        store.get_review(id).ok().flatten().into_iter().collect()
    } else {
        // All reviews (open + closed) — GC is about disk, not state.
        let mut all = Vec::new();
        // Walk every known repo name from the store's open list + closed
        // via a broad list: re-list per configured repo.
        for r in repos {
            if let Ok(rows) = store.list_reviews(&r.name, None) {
                all.extend(rows);
            }
        }
        all
    };
    for review in reviews {
        let Some(repo) = repos.iter().find(|r| r.name == review.repo) else {
            continue;
        };
        while store.patchset_count(review.id).unwrap_or(0) as u32 > max {
            let Some(old) = store.oldest_patchset(review.id).ok().flatten() else {
                break;
            };
            let _ = delete_patchset_ref(&repo.path, review.id, old.ps_number);
            if store
                .delete_patchset(review.id, old.ps_number)
                .unwrap_or(false)
            {
                deleted += 1;
            } else {
                break;
            }
        }
    }
    Ok(deleted)
}

// --- auto-capture worker ---------------------------------------------------

/// Background worker: listens for `repo.head_moved` on the EventBus and
/// auto-captures patchsets for open reviews on that repo, debounced.
/// Capture work is spawn_blocking so it never sits on the mirror's hot
/// path (the mirror only publishes the event; we consume it here).
pub fn spawn_auto_capture_worker(
    store: Arc<Store>,
    bus: Arc<EventBus>,
    repos: Vec<RepoEntry>,
    max_patchsets: u32,
    patchset_capture: bool,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        if !patchset_capture {
            tracing::info!(
                "kb-code: [review] patchset_capture = false — auto-capture idle \
                 (explicit POST /api/reviews/{{id}}/snapshot still works)"
            );
            return;
        }
        let mut rx = bus.subscribe();
        // review_id → last observed tip_sha that was different from latest ps
        let mut pending: HashMap<i64, (String, tokio::time::Instant)> = HashMap::new();
        let mut ticker = tokio::time::interval(Duration::from_secs(1));
        ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {
            tokio::select! {
                msg = rx.recv() => {
                    match msg {
                        Ok(env) if env.type_ == "repo.head_moved" => {
                            let repo_name = env.payload
                                .get("repo")
                                .and_then(|v| v.as_str())
                                .unwrap_or("")
                                .to_string();
                            if repo_name.is_empty() {
                                continue;
                            }
                            let Some(repo_entry) = repos.iter().find(|r| r.name == repo_name).cloned() else {
                                continue;
                            };
                            let store2 = store.clone();
                            let open = match tokio::task::spawn_blocking(move || {
                                store2.list_open_reviews_for_repo(&repo_name)
                            }).await {
                                Ok(Ok(rows)) => rows,
                                _ => continue,
                            };
                            for review in open {
                                let root = repo_entry.path.clone();
                                let head_ref = review.head_ref.clone();
                                let tip = match tokio::task::spawn_blocking(move || {
                                    resolve_commit_sha(&root, &parse_user_ref(&head_ref)?)
                                }).await {
                                    Ok(Ok(s)) => s,
                                    _ => continue,
                                };
                                let store3 = store.clone();
                                let rid = review.id;
                                let latest_tip = tokio::task::spawn_blocking(move || {
                                    store3.latest_patchset(rid).ok().flatten().map(|p| p.tip_sha)
                                })
                                .await
                                .unwrap_or_default();
                                let equals = latest_tip.as_deref() == Some(tip.as_str());
                                if !should_auto_capture(true, true, equals, false) {
                                    // tip equal → clear any pending
                                    if equals {
                                        pending.remove(&review.id);
                                    } else {
                                        // tip moved: (re)arm debounce
                                        pending.insert(review.id, (tip, tokio::time::Instant::now()));
                                    }
                                }
                            }
                        }
                        Ok(_) => {}
                        Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue,
                        Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
                    }
                }
                _ = ticker.tick() => {
                    let now = tokio::time::Instant::now();
                    let ready: Vec<(i64, String)> = pending
                        .iter()
                        .filter(|(_, (_, t0))| now.duration_since(*t0) >= AUTO_CAPTURE_DEBOUNCE)
                        .map(|(id, (tip, _))| (*id, tip.clone()))
                        .collect();
                    for (id, tip) in ready {
                        pending.remove(&id);
                        let store4 = store.clone();
                        let bus4 = bus.clone();
                        let repos4 = repos.clone();
                        let max = max_patchsets;
                        tokio::spawn(async move {
                            let _ = tokio::task::spawn_blocking(move || {
                                let Some(review) = store4.get_review(id).ok().flatten() else {
                                    return;
                                };
                                if review.state != "open" {
                                    return;
                                }
                                let Some(repo) = repos4.iter().find(|r| r.name == review.repo) else {
                                    return;
                                };
                                // Re-check tip still matches the pending one
                                // and still differs from latest ps.
                                let current = match parse_user_ref(&review.head_ref)
                                    .and_then(|r| resolve_commit_sha(&repo.path, &r))
                                {
                                    Ok(s) => s,
                                    Err(_) => return,
                                };
                                if current != tip {
                                    return; // moved again; another arm will fire
                                }
                                let latest = store4.latest_patchset(id).ok().flatten();
                                let equals = latest.as_ref().map(|p| p.tip_sha.as_str()) == Some(tip.as_str());
                                if !should_auto_capture(true, true, equals, true) {
                                    return;
                                }
                                match capture_patchset(&store4, &bus4, &repo.path, &review, max, true) {
                                    Ok(ps) => tracing::info!(
                                        review_id = id,
                                        ps = ps.ps_number,
                                        tip = %short_sha(&ps.tip_sha),
                                        "kb-code: auto-captured review patchset"
                                    ),
                                    Err(e) => tracing::warn!(
                                        review_id = id,
                                        error = %e,
                                        "kb-code: auto-capture failed"
                                    ),
                                }
                            }).await;
                        });
                    }
                }
            }
        }
    })
}

// --- wire shapes + routes --------------------------------------------------

#[derive(Debug, Deserialize)]
pub struct CreateReviewBody {
    pub repo: String,
    pub head_ref: String,
    #[serde(default)]
    pub base_ref: Option<String>,
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default)]
    pub session_id: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct PatchReviewBody {
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default)]
    pub state: Option<String>,
    /// PRR-R2 (design doc §2 row 6 / §4.2) — the artifact↔review join hint.
    /// A plain `Option<String>` (not the double-`Option` "was this key
    /// present at all" idiom `update_review`'s own `title` handling uses) —
    /// this route treats "field present with a string" as SET and "field
    /// absent (or present-but-null)" as UNCHANGED for `title`/`state`
    /// already, and `artifact_hint_kb`/`artifact_hint_id` follow the SAME
    /// convention for consistency, not a new one. The two fields are
    /// written TOGETHER as a pair (`Store::set_review_artifact_hint`
    /// overwrites both columns at once — a `kb`+`id` hint is one join, not
    /// two independent fields): sending only one clears the other. Never
    /// resolved/verified here — kb-code has no business validating a kb doc
    /// id against a schema it doesn't own (design doc §4.2); verification
    /// is `GET /api/reviews/{id}/artifact`'s live, unpersisted job.
    #[serde(default)]
    pub artifact_hint_kb: Option<String>,
    #[serde(default)]
    pub artifact_hint_id: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct ViewedBody {
    pub path: String,
    pub blob_sha: String,
}

#[derive(Debug, Deserialize)]
pub struct ListReviewsParams {
    pub repo: String,
    #[serde(default)]
    pub state: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct FilesParams {
    #[serde(default)]
    pub ps: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct InterdiffParams {
    pub from: i64,
    pub to: i64,
}

// 2026-08-31 incident (store.rs module doc) — `get_review` runs on the
// blocking pool via `run_blocking`; `state` stays borrowed across the
// `.await` (its own reference, not moved), so `find_repo`'s in-memory
// (non-store) lookup afterward still returns a borrow tied to `state`'s
// lifetime exactly as before. Every call site just gained `.await`.
pub(crate) async fn require_review(
    state: &SharedState,
    id: i64,
) -> Result<(ReviewRow, &RepoEntry, i64), ApiError> {
    let review = state
        .store
        .run_blocking(move |store| store.get_review(id))
        .await?
        .ok_or_else(|| ApiError::not_found(format!("no such review: {id}")))?;
    let (repo, repo_id) = find_repo(state, &review.repo)?;
    Ok((review, repo, repo_id))
}

/// `POST /api/reviews` — create + capture ps1. Loopback-only.
pub async fn create_review(
    State(state): State<SharedState>,
    Json(body): Json<CreateReviewBody>,
) -> Result<impl IntoResponse, ApiError> {
    let (repo, _repo_id) = find_repo(&state, &body.repo)?;
    reject_user_ref(&body.head_ref)?;
    if let Some(b) = body.base_ref.as_deref() {
        reject_user_ref(b)?;
    }
    let base_ref = body
        .base_ref
        .clone()
        .unwrap_or_else(|| default_base_ref(&repo.path));
    // Pre-resolve both ends so we fail clean before inserting a row.
    let root = repo.path.clone();
    let head = body.head_ref.clone();
    let base = base_ref.clone();
    tokio::task::spawn_blocking(move || {
        resolve_commit_sha(&root, &parse_user_ref(&head)?)?;
        resolve_commit_sha(&root, &parse_user_ref(&base)?)?;
        Ok::<(), ReviewGitError>(())
    })
    .await
    .map_err(|e| ApiError::new(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))??;

    let now = now_unix();
    let repo_name = body.repo.clone();
    let title = body.title.clone();
    let base_ref_c = base_ref.clone();
    let head_ref = body.head_ref.clone();
    let session_id = body.session_id.clone();
    // 2026-08-31 incident (store.rs module doc): coarse-wrap the two
    // sequential store calls (insert + re-fetch) in one blocking-pool trip.
    let review = state
        .store
        .run_blocking(move |store| -> Result<ReviewRow, ApiError> {
            let id = store.create_review(
                &repo_name,
                title.as_deref(),
                &base_ref_c,
                &head_ref,
                session_id.as_deref(),
                now,
            )?;
            store
                .get_review(id)?
                .ok_or_else(|| ApiError::new(StatusCode::INTERNAL_SERVER_ERROR, "review vanished"))
        })
        .await?;

    let store = state.store.clone();
    let bus = state.bus.clone();
    let root = repo.path.clone();
    let max = state.review.max_patchsets;
    let review2 = review.clone();
    let ps = tokio::task::spawn_blocking(move || {
        capture_patchset(&store, &bus, &root, &review2, max, false)
    })
    .await
    .map_err(|e| ApiError::new(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))??;

    Ok((
        StatusCode::CREATED,
        [(header::CACHE_CONTROL, "no-store")],
        Json(serde_json::json!({
            "schema": SCHEMA,
            "id": review.id,
            "repo": review.repo,
            "title": review.title,
            "base_ref": review.base_ref,
            "head_ref": review.head_ref,
            "session_id": review.session_id,
            "state": review.state,
            "created_at": review.created_at,
            "updated_at": review.updated_at,
            "latest_ps": ps.ps_number,
            "tip_sha": ps.tip_sha,
            "base_sha": ps.base_sha,
        })),
    ))
}

/// `POST /api/reviews/{id}/snapshot` — explicit capture. Loopback-only.
pub async fn snapshot_review(
    State(state): State<SharedState>,
    AxumPath(id): AxumPath<i64>,
) -> Result<impl IntoResponse, ApiError> {
    let (review, repo, _) = require_review(&state, id).await?;
    let store = state.store.clone();
    let bus = state.bus.clone();
    let root = repo.path.clone();
    let max = state.review.max_patchsets;
    let ps = tokio::task::spawn_blocking(move || {
        capture_patchset(&store, &bus, &root, &review, max, false)
    })
    .await
    .map_err(|e| ApiError::new(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))??;
    Ok((
        [(header::CACHE_CONTROL, "no-store")],
        Json(serde_json::json!({
            "schema": SCHEMA,
            "review_id": id,
            "ps_number": ps.ps_number,
            "tip_sha": ps.tip_sha,
            "base_sha": ps.base_sha,
            "captured_at": ps.captured_at,
        })),
    ))
}

/// `PATCH /api/reviews/{id}` — title / state. Loopback-only.
pub async fn patch_review(
    State(state): State<SharedState>,
    AxumPath(id): AxumPath<i64>,
    Json(body): Json<PatchReviewBody>,
) -> Result<impl IntoResponse, ApiError> {
    let (review, _, _) = require_review(&state, id).await?;
    if let Some(ref s) = body.state {
        if s != "open" && s != "closed" {
            return Err(ApiError::bad_request(format!(
                "state must be open|closed, got {s:?}"
            )));
        }
    }
    let title_owned = body.title.clone();
    let state_owned = body.state.clone();
    let now = now_unix();
    let ok = state
        .store
        .run_blocking(move |store| {
            let title = if title_owned.is_some() {
                Some(title_owned.as_deref())
            } else {
                None
            };
            store.update_review(id, title, state_owned.as_deref(), now)
        })
        .await?;
    if !ok {
        return Err(ApiError::not_found(format!("no such review: {id}")));
    }
    // PRR-R2 (design doc §2 row 6) — the artifact hint pair, written
    // together (see `PatchReviewBody::artifact_hint_kb`'s own doc).
    if body.artifact_hint_kb.is_some() || body.artifact_hint_id.is_some() {
        let hint_kb = body.artifact_hint_kb.clone();
        let hint_id = body.artifact_hint_id.clone();
        state
            .store
            .run_blocking(move |store| {
                store.set_review_artifact_hint(id, hint_kb.as_deref(), hint_id.as_deref())
            })
            .await?;
    }
    emit_review_changed(&state.bus, id, &review.repo, "meta", false);
    let (updated, binding) = state
        .store
        .run_blocking(move |store| -> Result<_, ApiError> {
            let updated = store
                .get_review(id)?
                .ok_or_else(|| ApiError::not_found(format!("no such review: {id}")))?;
            let binding = store.get_review_pr_binding(id)?.unwrap_or_default();
            Ok((updated, binding))
        })
        .await?;
    Ok((
        [(header::CACHE_CONTROL, "no-store")],
        Json(serde_json::json!({
            "schema": SCHEMA,
            "id": updated.id,
            "repo": updated.repo,
            "title": updated.title,
            "base_ref": updated.base_ref,
            "head_ref": updated.head_ref,
            "session_id": updated.session_id,
            "state": updated.state,
            "created_at": updated.created_at,
            "updated_at": updated.updated_at,
            "artifact_hint_kb": binding.artifact_hint_kb,
            "artifact_hint_id": binding.artifact_hint_id,
        })),
    ))
}

#[derive(Debug, Deserialize)]
pub struct SetVerdictBody {
    pub state: String,
    #[serde(default)]
    pub note: Option<String>,
}

/// `PUT /api/reviews/{id}/verdict` — set the review-pass verdict. S2-B
/// GATED (`review_mutations_gate`: loopback unconditionally, else `[review]
/// remote_mutations`, default OFF — see [`crate::review_gate::
/// review_mutations_gate`]). Allowed on closed reviews. A review with zero
/// patchsets is `400`. Identical `(state, note)` is a no-op
/// (`{changed:false}`, no SSE).
pub async fn put_verdict(
    State(state): State<SharedState>,
    AxumPath(id): AxumPath<i64>,
    Json(body): Json<SetVerdictBody>,
) -> Result<impl IntoResponse, ApiError> {
    let (review, _, _) = require_review(&state, id).await?;
    parse_verdict_state(&body.state)?;
    let verdict_state = body.state.clone();
    let note = body.note.clone();
    let now = now_unix();
    let changed = state
        .store
        .run_blocking(move |store| -> Result<bool, ApiError> {
            let latest = store
                .latest_patchset(id)?
                .ok_or_else(|| ApiError::bad_request(format!("review {id} has no patchsets")))?;
            store
                .set_review_verdict(id, &verdict_state, note.as_deref(), now, latest.ps_number)?
                .ok_or_else(|| ApiError::not_found(format!("no such review: {id}")))
        })
        .await?;
    if changed {
        emit_review_changed(&state.bus, id, &review.repo, "verdict", false);
    }
    Ok((
        [(header::CACHE_CONTROL, "no-store")],
        Json(serde_json::json!({ "changed": changed })),
    ))
}

/// `DELETE /api/reviews/{id}/verdict` — clear the four verdict columns.
/// S2-B GATED, same as [`put_verdict`] above. `{changed:false}` + no SSE
/// when already unset.
pub async fn delete_verdict(
    State(state): State<SharedState>,
    AxumPath(id): AxumPath<i64>,
) -> Result<impl IntoResponse, ApiError> {
    let (review, _, _) = require_review(&state, id).await?;
    let changed = state
        .store
        .run_blocking(move |store| -> Result<bool, ApiError> {
            store
                .clear_review_verdict(id)?
                .ok_or_else(|| ApiError::not_found(format!("no such review: {id}")))
        })
        .await?;
    if changed {
        emit_review_changed(&state.bus, id, &review.repo, "verdict", false);
    }
    Ok((
        [(header::CACHE_CONTROL, "no-store")],
        Json(serde_json::json!({ "changed": changed })),
    ))
}

/// `DELETE /api/reviews/{id}` — rows + refs. Loopback-only.
pub async fn delete_review(
    State(state): State<SharedState>,
    AxumPath(id): AxumPath<i64>,
) -> Result<impl IntoResponse, ApiError> {
    let (review, repo, _) = require_review(&state, id).await?;
    let store = state.store.clone();
    let bus = state.bus.clone();
    let root = repo.path.clone();
    tokio::task::spawn_blocking(move || delete_review_with_refs(&store, &bus, &root, &review))
        .await
        .map_err(|e| ApiError::new(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))??;
    Ok(StatusCode::NO_CONTENT)
}

/// `PUT /api/reviews/{id}/viewed` — mark a path viewed at blob_sha. Loopback.
pub async fn put_viewed(
    State(state): State<SharedState>,
    AxumPath(id): AxumPath<i64>,
    Json(body): Json<ViewedBody>,
) -> Result<impl IntoResponse, ApiError> {
    let (_review, _, _) = require_review(&state, id).await?;
    safe_rel_path(&body.path)?;
    if !body.blob_sha.is_empty() && !is_full_sha(&body.blob_sha) {
        return Err(ApiError::bad_request(format!(
            "blob_sha must be empty or 40-hex, got {:?}",
            body.blob_sha
        )));
    }
    let path = body.path.clone();
    let blob_sha = body.blob_sha.clone();
    let now = now_unix();
    state
        .store
        .run_blocking(move |store| store.upsert_viewed(id, &path, &blob_sha, now))
        .await?;
    Ok((
        [(header::CACHE_CONTROL, "no-store")],
        Json(serde_json::json!({
            "review_id": id,
            "path": body.path,
            "blob_sha": body.blob_sha,
        })),
    ))
}

/// `DELETE /api/reviews/{id}/viewed/{path}` — path is a single URL-encoded
/// segment. Loopback-only.
pub async fn delete_viewed(
    State(state): State<SharedState>,
    AxumPath((id, path)): AxumPath<(i64, String)>,
) -> Result<impl IntoResponse, ApiError> {
    let (_review, _, _) = require_review(&state, id).await?;
    // axum already percent-decodes path params.
    safe_rel_path(&path)?;
    let path_c = path.clone();
    let ok = state
        .store
        .run_blocking(move |store| store.delete_viewed(id, &path_c))
        .await?;
    if !ok {
        return Err(ApiError::not_found(format!(
            "no viewed entry for path {path:?}"
        )));
    }
    Ok(StatusCode::NO_CONTENT)
}

/// `POST /api/reviews/gc` — manual GC (same logic capture uses). Loopback.
#[derive(Debug, Deserialize)]
pub struct GcBody {
    #[serde(default)]
    pub review_id: Option<i64>,
}

pub async fn gc_reviews(
    State(state): State<SharedState>,
    Json(body): Json<GcBody>,
) -> Result<impl IntoResponse, ApiError> {
    let store = state.store.clone();
    let repos = state.repos.clone();
    let max = state.review.max_patchsets;
    let deleted =
        tokio::task::spawn_blocking(move || gc_patchsets(&store, &repos, body.review_id, max))
            .await
            .map_err(|e| ApiError::new(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))??;
    Ok((
        [(header::CACHE_CONTROL, "no-store")],
        Json(serde_json::json!({
            "schema": SCHEMA,
            "deleted": deleted,
            "max_patchsets": max,
        })),
    ))
}

/// The store+git compute behind `GET /api/reviews` — pure w.r.t. its
/// caller's async machinery (takes `&Store` + an already-fetched `rows`,
/// no `SharedState`/HTTP), so it runs entirely inside ONE `run_blocking`
/// trip AND is directly measurable/testable without a daemon (same "pure
/// composition, thin route" shape `review_inbox::compose_rows` /
/// `review_findings::recurring_findings` already establish in this crate).
///
/// PF-K1 — replaces the old per-review sequential-await loop (2026-08-31
/// incident doc, superseded here): `latest_patchset` / `get_review_pr_
/// binding` / `get_review_report` / `list_viewed` were each their own
/// round trip PER ROW. Every review id in `rows` is now batched through
/// ONE `IN (…)` query each (`store::latest_patchsets` /
/// `get_review_pr_bindings` / `get_review_reports` / `list_viewed_batch`).
/// The per-review git diff (`files_changed`) stays inherently per-review
/// (distinct `(base_sha, tip_sha)` pairs), but runs synchronously in THIS
/// one call rather than a separate `spawn_blocking` per row. `blob_sha_at`
/// is memoized by `(path, tip_sha)` in `blob_sha_cache`, shared across
/// every review in `rows` (a real hit whenever two reviews share a tip) —
/// but the VIEWED-credit GATE stays scoped to each review's OWN diff paths
/// (a fresh per-review `blob_map`, built from the shared cache's values),
/// so a path present in one review's diff can never leak "viewed" credit
/// into a different review that doesn't have that path in its own diff,
/// even if the two reviews happen to share a `tip_sha`.
/// `repo_id` is a single value for the whole call (`list_reviews` scopes
/// to one repo), so `open_annotations` is computed as ONE `open_annotation_
/// counts_by_path` call over the UNION of every review's changed paths,
/// then summed per review over its own DEDUPED path set — `IN (…)`
/// counting semantics never double-count a duplicate path, so dedup before
/// summing is required for byte-identical parity with the old per-review
/// `COUNT(*) … path IN (…)` call.
pub fn compose_review_list_rows(
    store: &Store,
    repo_root: &Path,
    repo_id: i64,
    rows: Vec<ReviewRow>,
) -> Result<Vec<serde_json::Value>, ApiError> {
    if rows.is_empty() {
        return Ok(Vec::new());
    }
    let review_ids: Vec<i64> = rows.iter().map(|r| r.id).collect();

    let latest_map = store.latest_patchsets(&review_ids)?;
    let binding_map = store.get_review_pr_bindings(&review_ids)?;
    let report_map = store.get_review_reports(&review_ids)?;
    let viewed_map = store.list_viewed_batch(&review_ids)?;

    // Per-review git diff + per-review viewed-credit gate (see the doc
    // above for why `blob_map` must stay scoped to each review's own
    // paths even though the underlying `blob_sha_at` value is memoized
    // globally).
    let mut blob_sha_cache: HashMap<(String, String), String> = HashMap::new();
    #[derive(Default)]
    struct DiffInfo {
        files_len: usize,
        paths: Vec<String>,
        blob_map: HashMap<String, String>,
    }
    let mut diffs: HashMap<i64, DiffInfo> = HashMap::with_capacity(rows.len());
    for review in &rows {
        let Some(ps) = latest_map.get(&review.id) else {
            diffs.insert(review.id, DiffInfo::default());
            continue;
        };
        let files = files_changed(repo_root, &ps.base_sha, &ps.tip_sha).unwrap_or_default();
        let paths: Vec<String> = files.iter().map(|f| f.path.clone()).collect();
        let mut blob_map: HashMap<String, String> = HashMap::with_capacity(paths.len());
        for p in &paths {
            let key = (p.clone(), ps.tip_sha.clone());
            let sha = blob_sha_cache
                .entry(key)
                .or_insert_with(|| blob_sha_at(repo_root, &ps.tip_sha, p).unwrap_or_default())
                .clone();
            blob_map.insert(p.clone(), sha);
        }
        diffs.insert(
            review.id,
            DiffInfo {
                files_len: files.len(),
                paths,
                blob_map,
            },
        );
    }

    // ONE open-annotation-count query over the union of every review's
    // changed paths (same `repo_id` for the whole request) instead of one
    // per review.
    let mut union_paths: Vec<String> = diffs
        .values()
        .flat_map(|d| d.paths.iter().cloned())
        .collect();
    union_paths.sort_unstable();
    union_paths.dedup();
    let ann_counts = store.open_annotation_counts_by_path(repo_id, &union_paths)?;

    let mut out = Vec::with_capacity(rows.len());
    for review in rows {
        let latest = latest_map.get(&review.id).cloned();
        let diff = diffs.remove(&review.id).unwrap_or_default();
        let viewed_count = if latest.is_some() {
            let viewed_rows = viewed_map.get(&review.id).cloned().unwrap_or_default();
            viewed_rows
                .iter()
                .filter(|v| diff.blob_map.get(&v.path).is_some_and(|b| b == &v.blob_sha))
                .count()
        } else {
            0
        };
        let mut unique_paths = diff.paths.clone();
        unique_paths.sort_unstable();
        unique_paths.dedup();
        let open_ann: i64 = unique_paths
            .iter()
            .map(|p| ann_counts.get(p).copied().unwrap_or(0))
            .sum();

        let (verdict, verdict_stale) = verdict_block(&review, latest.as_ref().map(|p| p.ps_number));
        // PRR-R4 (R2 leftover) — surface the PR binding + report summary on
        // every list row too, not just `GET /reviews/{id}` (see
        // `pr_binding_and_report_fields`'s own doc).
        let binding = binding_map.get(&review.id).cloned().unwrap_or_default();
        let report = report_map.get(&review.id).cloned().unwrap_or_default();

        let mut row = serde_json::json!({
            "id": review.id,
            "repo": review.repo,
            "title": review.title,
            "base_ref": review.base_ref,
            "head_ref": review.head_ref,
            "session_id": review.session_id,
            "state": review.state,
            "created_at": review.created_at,
            "updated_at": review.updated_at,
            "latest_ps": latest.as_ref().map(|p| p.ps_number),
            "files_count": diff.files_len,
            "viewed_count": viewed_count,
            "open_annotations": open_ann,
            "verdict": verdict,
            "verdict_stale": verdict_stale,
        });
        merge_pr_binding_and_report_fields(&mut row, &binding, &report);
        out.push(row);
    }
    Ok(out)
}

/// `GET /api/reviews?repo=[&state=]`
pub async fn list_reviews(
    State(state): State<SharedState>,
    Query(params): Query<ListReviewsParams>,
) -> Result<impl IntoResponse, ApiError> {
    let (repo, repo_id) = find_repo(&state, &params.repo)?;
    if let Some(s) = params.state.as_deref() {
        if s != "open" && s != "closed" {
            return Err(ApiError::bad_request(format!(
                "state must be open|closed, got {s:?}"
            )));
        }
    }
    let repo_name = params.repo.clone();
    let state_filter = params.state.clone();
    let root = repo.path.clone();
    // PF-K1 (2026-08-31 incident doc, store.rs) — the whole list compose
    // (store batch fan-out + per-review git diff + CPU-only aggregation)
    // is now ONE blocking-pool trip, down from one initial fetch plus up
    // to three more PER REVIEW.
    let out = state
        .store
        .run_blocking(move |store| -> Result<_, ApiError> {
            let rows = store.list_reviews(&repo_name, state_filter.as_deref())?;
            compose_review_list_rows(store, &root, repo_id, rows)
        })
        .await?;
    Ok((
        [(header::CACHE_CONTROL, "no-store")],
        Json(serde_json::json!({
            "schema": SCHEMA,
            "reviews": out,
        })),
    ))
}

/// `GET /api/reviews/{id}`
pub async fn get_review(
    State(state): State<SharedState>,
    AxumPath(id): AxumPath<i64>,
) -> Result<impl IntoResponse, ApiError> {
    let (review, repo, _) = require_review(&state, id).await?;
    let pss = state
        .store
        .run_blocking(move |store| store.list_patchsets(id))
        .await?;
    let root = repo.path.clone();
    let mut patchsets = Vec::with_capacity(pss.len());
    for ps in pss {
        let root2 = root.clone();
        let base = ps.base_sha.clone();
        let tip = ps.tip_sha.clone();
        let count = tokio::task::spawn_blocking(move || commit_count(&root2, &base, &tip))
            .await
            .map_err(|e| ApiError::new(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?
            .unwrap_or(0);
        patchsets.push(serde_json::json!({
            "ps_number": ps.ps_number,
            "tip_sha": short_sha(&ps.tip_sha),
            "tip_sha_full": ps.tip_sha,
            "base_sha": short_sha(&ps.base_sha),
            "base_sha_full": ps.base_sha,
            "captured_at": ps.captured_at,
            "commit_count": count,
        }));
    }
    let latest_ps = patchsets
        .last()
        .and_then(|p| p.get("ps_number"))
        .and_then(|v| v.as_i64());
    let (verdict, verdict_stale) = verdict_block(&review, latest_ps);
    // PRR-R4 (R2 leftover) — see `pr_binding_and_report_fields`'s own doc.
    let (binding, report) = state
        .store
        .run_blocking(move |store| -> Result<_, ApiError> {
            let binding = store.get_review_pr_binding(id)?.unwrap_or_default();
            let report = store.get_review_report(id)?.unwrap_or_default();
            Ok((binding, report))
        })
        .await?;
    let mut body = serde_json::json!({
        "schema": SCHEMA,
        "id": review.id,
        "repo": review.repo,
        "title": review.title,
        "base_ref": review.base_ref,
        "head_ref": review.head_ref,
        "session_id": review.session_id,
        "state": review.state,
        "created_at": review.created_at,
        "updated_at": review.updated_at,
        "patchsets": patchsets,
        "verdict": verdict,
        "verdict_stale": verdict_stale,
    });
    merge_pr_binding_and_report_fields(&mut body, &binding, &report);
    Ok(([(header::CACHE_CONTROL, "no-store")], Json(body)))
}

/// Resolve `?ps=` (or latest). Shared by `/files`, `/risk`, `/map`, `/reading-order`.
pub(crate) fn resolve_ps(
    store: &Store,
    review_id: i64,
    ps: Option<&str>,
) -> Result<ReviewPatchsetRow, ApiError> {
    match ps {
        None | Some("latest") | Some("") => store
            .latest_patchset(review_id)?
            .ok_or_else(|| ApiError::not_found("review has no patchsets")),
        Some(s) => {
            let n: i64 = s
                .parse()
                .map_err(|_| ApiError::bad_request(format!("invalid ps: {s:?}")))?;
            store
                .get_patchset(review_id, n)?
                .ok_or_else(|| ApiError::not_found(format!("no patchset {n}")))
        }
    }
}

/// `GET /api/reviews/{id}/files?ps=`
pub async fn review_files(
    State(state): State<SharedState>,
    AxumPath(id): AxumPath<i64>,
    Query(params): Query<FilesParams>,
) -> Result<impl IntoResponse, ApiError> {
    let (_review, repo, repo_id) = require_review(&state, id).await?;
    let ps_param = params.ps.clone();
    let ps = state
        .store
        .run_blocking(move |store| resolve_ps(store, id, ps_param.as_deref()))
        .await?;
    let root = repo.path.clone();
    let base = ps.base_sha.clone();
    let tip = ps.tip_sha.clone();
    let files = tokio::task::spawn_blocking(move || files_changed(&root, &base, &tip))
        .await
        .map_err(|e| ApiError::new(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))??;

    let paths: Vec<String> = files.iter().map(|f| f.path.clone()).collect();
    let (viewed, ann_counts) = state
        .store
        .run_blocking(move |store| -> Result<_, ApiError> {
            let viewed = store.list_viewed(id)?;
            let ann_counts = store.open_annotation_counts_by_path(repo_id, &paths)?;
            Ok((viewed, ann_counts))
        })
        .await?;
    let viewed_map: HashMap<String, String> =
        viewed.into_iter().map(|v| (v.path, v.blob_sha)).collect();

    let mut out = Vec::with_capacity(files.len());
    for f in &files {
        let root = repo.path.clone();
        let tip = ps.tip_sha.clone();
        let path = f.path.clone();
        let blob = tokio::task::spawn_blocking(move || blob_sha_at(&root, &tip, &path))
            .await
            .map_err(|e| ApiError::new(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?
            .unwrap_or_default();
        let viewed_blob = viewed_map.get(&f.path);
        let (viewed_flag, viewed_stale) = match viewed_blob {
            Some(vb) if vb == &blob => (true, false),
            Some(_) => (true, true), // row exists but content changed
            None => (false, false),
        };
        out.push(serde_json::json!({
            "path": f.path,
            "old_path": f.old_path,
            "status": f.status,
            "additions": f.insertions,
            "deletions": f.deletions,
            "blob_sha": blob,
            "viewed": viewed_flag,
            "viewed_stale": viewed_stale,
            "open_annotations": ann_counts.get(&f.path).copied().unwrap_or(0),
        }));
    }
    Ok((
        [(header::CACHE_CONTROL, "no-store")],
        Json(serde_json::json!({
            "schema": SCHEMA,
            "review_id": id,
            "ps_number": ps.ps_number,
            "base_sha": ps.base_sha,
            "tip_sha": ps.tip_sha,
            "files": out,
        })),
    ))
}

/// `GET /api/reviews/{id}/interdiff?from=&to=`
pub async fn review_interdiff(
    State(state): State<SharedState>,
    AxumPath(id): AxumPath<i64>,
    Query(params): Query<InterdiffParams>,
) -> Result<impl IntoResponse, ApiError> {
    let (_review, repo, _) = require_review(&state, id).await?;
    let (from, to) = (params.from, params.to);
    let (from_ps, to_ps) = state
        .store
        .run_blocking(move |store| -> Result<_, ApiError> {
            let from_ps = store
                .get_patchset(id, from)?
                .ok_or_else(|| ApiError::not_found(format!("no patchset {from}")))?;
            let to_ps = store
                .get_patchset(id, to)?
                .ok_or_else(|| ApiError::not_found(format!("no patchset {to}")))?;
            Ok((from_ps, to_ps))
        })
        .await?;

    let root = repo.path.clone();
    let from_tip = from_ps.tip_sha.clone();
    let to_tip = to_ps.tip_sha.clone();
    let files = {
        let r = root.clone();
        let a = from_tip.clone();
        let b = to_tip.clone();
        tokio::task::spawn_blocking(move || {
            // Interdiff files = name-status+numstat between the two TIPs.
            if !is_full_sha(&a) || !is_full_sha(&b) {
                return Err(ReviewGitError::BadSha(format!("{a}..{b}")));
            }
            let range = format!("{a}..{b}");
            history::diff_files(&r, "diff", &["-M", &range]).map_err(Into::into)
        })
        .await
        .map_err(|e| ApiError::new(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))??
    };

    let range_diff = {
        let r = root.clone();
        // Four daemon-resolved shas from the patchset rows — never caller
        // text, so `Revspec::trusted` (V70-A2, SEC-17) is the honest
        // constructor; the ranges are still assembled by `RefRange`
        // rather than by hand.
        let old_range = crate::git::RefRange::new(
            crate::git::Revspec::trusted(from_ps.base_sha.clone()),
            crate::git::Revspec::trusted(from_ps.tip_sha.clone()),
            false,
        );
        let new_range = crate::git::RefRange::new(
            crate::git::Revspec::trusted(to_ps.base_sha.clone()),
            crate::git::Revspec::trusted(to_ps.tip_sha.clone()),
            false,
        );
        tokio::task::spawn_blocking(move || {
            history::range_diff::range_diff(&r, &old_range, &new_range)
        })
        .await
        .map_err(|e| ApiError::new(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?
        .map_err(|e| ApiError::bad_request(e.to_string()))?
    };

    let files_out: Vec<_> = files
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

    Ok((
        [(header::CACHE_CONTROL, "no-store")],
        Json(serde_json::json!({
            "schema": SCHEMA,
            "review_id": id,
            "from": params.from,
            "to": params.to,
            "from_tip": from_ps.tip_sha,
            "to_tip": to_ps.tip_sha,
            "files": files_out,
            "range_diff": {
                "pairs": range_diff.pairs,
                "truncated": range_diff.truncated,
            },
        })),
    ))
}

/// `GET /api/reviews/{id}/annotations` — open annotations on latest
/// patchset's change set. Carry-forward = existing live re-anchoring
/// (`stale` → exposed as `orphaned` for the review surface).
pub async fn review_annotations(
    State(state): State<SharedState>,
    AxumPath(id): AxumPath<i64>,
) -> Result<impl IntoResponse, ApiError> {
    let (review, repo, repo_id) = require_review(&state, id).await?;
    let ps = state
        .store
        .run_blocking(move |store| store.latest_patchset(id))
        .await?
        .ok_or_else(|| ApiError::not_found("review has no patchsets"))?;
    let root = repo.path.clone();
    let base = ps.base_sha.clone();
    let tip = ps.tip_sha.clone();
    let files = tokio::task::spawn_blocking(move || files_changed(&root, &base, &tip))
        .await
        .map_err(|e| ApiError::new(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))??;
    let paths: Vec<String> = files.iter().map(|f| f.path.clone()).collect();
    let rows = state
        .store
        .run_blocking(move |store| store.list_open_annotations_on_paths(repo_id, &paths))
        .await?;

    // Group by path; expose orphaned (= stale from live re-anchor) if we
    // can resolve cheaply. For the review surface we re-resolve against
    // the working tree like list_open_annotations does.
    let mut by_path: HashMap<String, Vec<serde_json::Value>> = HashMap::new();
    let mut content_cache: HashMap<String, String> = HashMap::new();
    for row in rows {
        let content = if let Some(c) = content_cache.get(&row.path) {
            c.clone()
        } else {
            // V70-A2 (SEC-13) — same containment as `routes`' annotation
            // re-anchor read: a stored path is a once-user-supplied path.
            let c = crate::security::paths::contained_abs_path(&repo.path, &row.path)
                .ok()
                .and_then(|abs| std::fs::read_to_string(abs).ok())
                .unwrap_or_default();
            content_cache.insert(row.path.clone(), c.clone());
            c
        };
        // Resolve via annotations::resolve when there's a selection anchor.
        let orphaned = if let Some(anchor_json) = row.anchor.as_deref() {
            if let Ok(anchor) = serde_json::from_str::<kb_core::review::Anchor>(anchor_json) {
                let r = crate::annotations::resolve(&content, &anchor);
                r.stale
            } else {
                false
            }
        } else {
            false
        };
        by_path
            .entry(row.path.clone())
            .or_default()
            .push(serde_json::json!({
                "id": row.id,
                "path": row.path,
                "intent": row.intent,
                "body": row.body,
                "author": row.author,
                "created_at": row.created_at,
                "orphaned": orphaned,
            }));
    }

    let groups: Vec<_> = by_path
        .into_iter()
        .map(|(path, annotations)| serde_json::json!({ "path": path, "annotations": annotations }))
        .collect();

    Ok((
        [(header::CACHE_CONTROL, "no-store")],
        Json(serde_json::json!({
            "schema": SCHEMA,
            "review_id": id,
            "repo": review.repo,
            "ps_number": ps.ps_number,
            "groups": groups,
        })),
    ))
}

// --- V3.2-B2 review-risk composite ----------------------------------------

pub const RISK_SCHEMA: &str = "review-risk/1";

/// `GET /api/reviews/{id}/risk` — per-file attention composite over the
/// latest patchset. Counters only at query time (path_stats / author_stats /
/// session_signals); one git diff for the change set (same as `/files`).
/// Never a quality verdict — terms are the substance.
pub async fn review_risk_route(
    State(state): State<SharedState>,
    AxumPath(id): AxumPath<i64>,
) -> Result<impl IntoResponse, ApiError> {
    let (review, repo, repo_id) = require_review(&state, id).await?;
    let ps = state
        .store
        .run_blocking(move |store| resolve_ps(store, id, None))
        .await?;
    let root = repo.path.clone();
    let base = ps.base_sha.clone();
    let tip = ps.tip_sha.clone();
    let store = state.store.clone();
    let repo_name = review.repo.clone();
    let session_id = review.session_id.clone();

    let out = tokio::task::spawn_blocking(move || {
        review_risk_sync(
            &store,
            repo_id,
            &root,
            &repo_name,
            id,
            ps.ps_number,
            &base,
            &tip,
            session_id.as_deref(),
        )
    })
    .await
    .map_err(|e| ApiError::new(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))??;

    Ok(([(header::CACHE_CONTROL, "no-store")], Json(out)))
}

#[allow(clippy::too_many_arguments)] // pure fan-in from one route; a struct would just rename locals
fn review_risk_sync(
    store: &Store,
    repo_id: i64,
    repo_root: &Path,
    repo_name: &str,
    review_id: i64,
    ps_number: i64,
    base_sha: &str,
    tip_sha: &str,
    review_session_id: Option<&str>,
) -> Result<serde_json::Value, ApiError> {
    use crate::behavioral::{
        hotspot_rank_norm, is_session_author, pain_from_signals, relative_churn, review_risk_score,
        session_id_from_author, stored_fail_term, RiskTermInputs, MAJOR_SHARE_THRESHOLD,
    };

    let files = files_changed(repo_root, base_sha, tip_sha)?;

    // Hotspot ranks from path_stats counters only (no git history walk).
    let path_rows = store.list_path_stats(repo_id)?;
    let churns: Vec<u64> = path_rows
        .iter()
        .map(|r| (r.lines_added + r.lines_deleted).max(0) as u64)
        .collect();
    let ranks = crate::behavioral::dense_ranks_desc(&churns);
    let total_paths = path_rows.len();
    let rank_by_path: HashMap<String, u32> = path_rows
        .iter()
        .enumerate()
        .map(|(i, r)| (r.path.clone(), ranks[i]))
        .collect();

    let mut file_out = Vec::with_capacity(files.len());
    for f in &files {
        let loc = crate::behavioral::complexity_for_path(repo_root, &f.path).loc;
        let rel_churn = relative_churn(f.insertions as i64, f.deletions as i64, loc);

        let authors = store.author_stats_for(repo_id, &f.path).unwrap_or_default();
        let humans: Vec<_> = authors
            .iter()
            .filter(|a| !is_session_author(&a.author))
            .collect();
        let total_h: i64 = humans.iter().map(|a| a.commits).sum();
        let ownership_minor = if total_h <= 0 {
            None
        } else {
            let minor_share: f64 = humans
                .iter()
                .map(|a| a.commits as f64 / total_h as f64)
                .filter(|&s| s <= MAJOR_SHARE_THRESHOLD)
                .sum();
            Some(minor_share.clamp(0.0, 1.0))
        };

        let hotspot_rank = rank_by_path
            .get(&f.path)
            .map(|&r| hotspot_rank_norm(r, total_paths));

        // agent_first_touch: producing session is the earliest session:* row.
        let agent_first_touch = match review_session_id {
            Some(sid) => {
                let earliest = store
                    .earliest_session_author(repo_id, &f.path)
                    .ok()
                    .flatten();
                match earliest {
                    Some((author, _)) => Some(session_id_from_author(&author) == Some(sid)),
                    None => {
                        // No session authors on path at all.
                        let has_this = authors
                            .iter()
                            .any(|a| session_id_from_author(&a.author) == Some(sid));
                        if has_this {
                            Some(true)
                        } else {
                            None
                        }
                    }
                }
            }
            None => None,
        };

        let session_pain = match review_session_id {
            Some(sid) => store
                .session_signals_for(repo_id, sid)
                .ok()
                .flatten()
                .map(|s| pain_from_signals(s.error_count, stored_fail_term(s.fail_count)).score),
            None => None,
        };

        let inputs = RiskTermInputs {
            relative_churn: rel_churn,
            ownership_minor,
            hotspot_rank,
            agent_first_touch,
            session_pain,
        };
        let risk = review_risk_score(&inputs);
        let (risk_json, inputs_missing) = match risk {
            Some(r) => {
                let missing: Vec<String> =
                    r.inputs_missing.iter().map(|s| (*s).to_string()).collect();
                (
                    serde_json::json!({
                        "score": r.score,
                        "terms": {
                            "relative_churn": r.terms.relative_churn,
                            "ownership_minor": r.terms.ownership_minor,
                            "hotspot_rank": r.terms.hotspot_rank,
                            "agent_first_touch": r.terms.agent_first_touch,
                            "session_pain": r.terms.session_pain,
                        },
                    }),
                    missing,
                )
            }
            None => (
                serde_json::Value::Null,
                vec![
                    "relative_churn".into(),
                    "ownership_minor".into(),
                    "hotspot_rank".into(),
                    "agent_first_touch".into(),
                    "session_pain".into(),
                ],
            ),
        };

        file_out.push(serde_json::json!({
            "path": f.path,
            "risk": risk_json,
            "inputs_missing": inputs_missing,
        }));
    }

    Ok(serde_json::json!({
        "schema": RISK_SCHEMA,
        "review_id": review_id,
        "repo": repo_name,
        "ps_number": ps_number,
        "files": file_out,
        "note": "Ranks ATTENTION for review triage — not code quality. \
                 Terms are the substance; score is a documented weighted sum \
                 of AVAILABLE terms only, renormalized over what exists. \
                 Missing inputs are named in inputs_missing; risk is null \
                 when nothing is computable (never a default 0 that reads as fine).",
    }))
}

// ── PRR-R2: PR binding routes + report + artifact hint ──────────────────
//
// kb v0.39 "The PR Room" (T2), unit R2 — Phase 2 of design-server.md
// (routes 1–7) plus the addendum-2 GitHub-conversation raw reads
// (`list_reviews`/`mergeStateStatus`, wired in `github.rs`/`routes.rs`).
// Builds ON PRR-R1's store layer (`get_review_pr_binding`/
// `set_review_pr_binding`/`set_review_pr_meta`/`set_review_artifact_hint`/
// `get_review_report`/`set_review_report`, already shipped) — this unit
// adds ONLY the HTTP surface + one new store lookup
// (`get_review_by_pr_binding`, the duplicate-binding pre-check).
//
// Design fill-in (flagged here for a later phase to revisit if wrong):
// `create_review_pr`'s git-fetch step (`fetch_pr_ref`, reused from
// `prs_fetch_route`) is LOAD-BEARING but deliberately does NOT gate on
// `github::github_repo` succeeding — mirroring `prs_fetch_route`'s own
// documented precedent ("the fetch mechanics themselves are host-agnostic").
// `github_repo` resolution is instead used ONLY to decide (a) `pr_repo_slug`
// (falls back to the raw configured origin URL, or `"unknown"` with no
// origin at all, when it isn't recognisably GitHub) and (b) whether the
// GitHub metadata-enrichment call is attempted at all (skipped, degrading
// exactly like any other enrichment failure, when the origin isn't
// GitHub-shaped). This lets `create_review_pr` bind a review to a PR number
// fetched from ANY git host — same flexibility `prs_fetch_route` already
// has — while GitHub-specific metadata naturally only works when the origin
// really is github.com.

/// `POST /api/reviews/pr` body (design doc §2 row 1).
#[derive(Debug, Deserialize)]
pub struct CreateReviewPrBody {
    pub repo: String,
    pub pr_number: u32,
    #[serde(default)]
    pub base_ref: Option<String>,
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default)]
    pub session_id: Option<String>,
}

/// `POST /api/reviews/pr` — create a review bound to a GitHub PR + capture
/// ps1 off the fetched ref. LOOPBACK-ONLY (see the module doc + `router.rs`).
/// See the PRR-R2 section doc above for the git-fetch/github_repo decoupling.
pub async fn create_review_pr(
    State(state): State<SharedState>,
    Json(body): Json<CreateReviewPrBody>,
) -> Result<axum::response::Response, ApiError> {
    let (repo, _repo_id) = find_repo(&state, &body.repo)?;
    let repo_root = repo.path.clone();

    // Duplicate-binding pre-check — 409 pointing at the existing review id
    // (design doc §2 row 1).
    let dup_repo = body.repo.clone();
    let dup_pr_number = body.pr_number as i64;
    if let Some(existing) = state
        .store
        .run_blocking(move |store| store.get_review_by_pr_binding(&dup_repo, dup_pr_number))
        .await?
    {
        return Ok((
            StatusCode::CONFLICT,
            [(header::CACHE_CONTROL, "no-store")],
            Json(serde_json::json!({
                "error": format!(
                    "review already bound to {}#{}",
                    body.repo, body.pr_number
                ),
                "existing_review_id": existing.id,
            })),
        )
            .into_response());
    }

    // Resolve owner/repo up front — used for `pr_repo_slug` and to decide
    // whether metadata enrichment is attempted at all (see the section doc).
    let repo_root_for_origin = repo_root.clone();
    let gh_repo_result =
        tokio::task::spawn_blocking(move || crate::github::github_repo(&repo_root_for_origin))
            .await
            .map_err(|e| {
                ApiError::new(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    format!("origin lookup task panicked: {e}"),
                )
            })?;
    let (gh_repo, pr_repo_slug) = match gh_repo_result {
        Ok(r) => {
            let slug = format!("{}/{}", r.owner, r.name);
            (Some(r), slug)
        }
        Err(crate::github::GithubError::NotGithubOrigin(url)) => (None, url),
        Err(_) => (None, "unknown".to_string()),
    };

    // The git fetch is LOAD-BEARING — 400 on failure, before any row is
    // written (design doc §2 row 1). Deliberately NOT `?` through
    // `impl From<GithubError> for ApiError` (that impl maps `FetchFailed`
    // to a 500 for `prs_fetch_route`'s own established contract) — here a
    // failed fetch is caller-attributable (bad PR number / unreachable
    // repo), so it is mapped to 400 explicitly for THIS route only.
    let number = body.pr_number;
    let root_for_fetch = repo_root.clone();
    let (target_ref, fetched_sha) =
        tokio::task::spawn_blocking(move || crate::github::fetch_pr_ref(&root_for_fetch, number))
            .await
            .map_err(|e| {
                ApiError::new(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    format!("fetch task panicked: {e}"),
                )
            })?
            .map_err(|e| ApiError::bad_request(format!("PR fetch failed: {e}")))?;

    // Pre-resolve base so we fail clean before inserting (mirrors
    // `create_review`'s own precedent).
    let base_ref = body
        .base_ref
        .clone()
        .unwrap_or_else(|| default_base_ref(&repo.path));
    if let Some(b) = body.base_ref.as_deref() {
        reject_user_ref(b)?;
    }
    let root_for_base = repo.path.clone();
    let base_for_resolve = base_ref.clone();
    tokio::task::spawn_blocking(move || {
        resolve_commit_sha(&root_for_base, &parse_user_ref(&base_for_resolve)?)
    })
    .await
    .map_err(|e| ApiError::new(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))??;

    let now = now_unix();
    let repo_name = body.repo.clone();
    let title = body.title.clone();
    let base_ref_c = base_ref.clone();
    let target_ref_c = target_ref.clone();
    let session_id = body.session_id.clone();
    // 2026-08-31 incident (store.rs module doc): coarse-wrap the two
    // sequential store calls (insert + re-fetch) in one blocking-pool trip.
    let review = state
        .store
        .run_blocking(move |store| -> Result<ReviewRow, ApiError> {
            let id = store.create_review(
                &repo_name,
                title.as_deref(),
                &base_ref_c,
                &target_ref_c,
                session_id.as_deref(),
                now,
            )?;
            store
                .get_review(id)?
                .ok_or_else(|| ApiError::new(StatusCode::INTERNAL_SERVER_ERROR, "review vanished"))
        })
        .await?;

    let store = state.store.clone();
    let bus = state.bus.clone();
    let root = repo.path.clone();
    let max = state.review.max_patchsets;
    let review2 = review.clone();
    let ps = tokio::task::spawn_blocking(move || {
        capture_patchset(&store, &bus, &root, &review2, max, false)
    })
    .await
    .map_err(|e| ApiError::new(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))??;

    // Best-effort GitHub metadata enrichment (design doc §2 row 1 /
    // §1.2's `pr_meta_json` shape). Only attempted when the origin
    // resolved as GitHub; any failure (including "not GitHub") degrades to
    // `pr_meta_json=null` + a reason — the review is already created either
    // way.
    let (pr_meta_json, pr_meta_unavailable_reason) = if let Some(ref gh) = gh_repo {
        match state
            .github
            .get_pull(&gh.owner, &gh.name, number as u64)
            .await
        {
            Ok(pull) => {
                let checks = match state
                    .github
                    .list_checks(&gh.owner, &gh.name, &pull.head_sha)
                    .await
                {
                    Ok(mut list) => {
                        list.truncate(crate::github::MAX_CHECKS);
                        list
                    }
                    // Checks are a SUB-part of the enrichment snapshot — a
                    // failure here degrades to an empty checks array rather
                    // than sinking the whole (already-successful) PR
                    // metadata fetch.
                    Err(_) => Vec::new(),
                };
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
                    // V70-A3X — kept in lock-step with `review_sweep.rs`'s
                    // own `meta` snapshot below.
                    "body": pull.body,
                    "checks": checks,
                });
                let meta_str = serde_json::to_string(&meta)
                    .map_err(|e| ApiError::new(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
                (Some(meta_str), None)
            }
            Err(e) => (None, Some(e.to_string())),
        }
    } else {
        (
            None,
            Some(
                "repo origin is not a recognized GitHub remote; \
                 metadata enrichment skipped"
                    .to_string(),
            ),
        )
    };

    // `pr_head_sha` is always the LOCALLY fetched ref's sha (guaranteed
    // available — the fetch above is load-bearing) rather than solely
    // GitHub's own reported `head.sha`: they are the same commit by
    // construction (`refs/pull/<n>/head` IS the PR head), and this way the
    // column is never null just because metadata enrichment degraded.
    let review_id = review.id;
    let pr_number_c = number as i64;
    let pr_repo_slug_c = pr_repo_slug.clone();
    let fetched_sha_c = fetched_sha.clone();
    let pr_meta_json_c = pr_meta_json.clone();
    state
        .store
        .run_blocking(move |store| {
            store.set_review_pr_binding(
                review_id,
                pr_number_c,
                &pr_repo_slug_c,
                Some(&fetched_sha_c),
                pr_meta_json_c.as_deref(),
                pr_meta_json_c.as_ref().map(|_| now),
            )
        })
        .await?;

    emit_review_changed(&state.bus, review.id, &review.repo, "pr_bound", false);

    let pr_meta_value: Option<serde_json::Value> = pr_meta_json
        .as_deref()
        .and_then(|s| serde_json::from_str(s).ok());

    Ok((
        StatusCode::CREATED,
        [(header::CACHE_CONTROL, "no-store")],
        Json(serde_json::json!({
            "schema": SCHEMA,
            "id": review.id,
            "repo": review.repo,
            "title": review.title,
            "base_ref": review.base_ref,
            "head_ref": review.head_ref,
            "session_id": review.session_id,
            "state": review.state,
            "created_at": review.created_at,
            "updated_at": review.updated_at,
            "latest_ps": ps.ps_number,
            "tip_sha": ps.tip_sha,
            "base_sha": ps.base_sha,
            "pr_number": number,
            "pr_repo_slug": pr_repo_slug,
            "pr_head_sha": fetched_sha,
            "pr_meta": pr_meta_value,
            "pr_meta_unavailable_reason": pr_meta_unavailable_reason,
        })),
    )
        .into_response())
}

/// `GET /api/reviews/{id}/report` (design doc §2 row 4) — the agent-authored
/// review report (`report_json`), read back verbatim. `{report: null}` when
/// none has been authored yet. Bearer.
pub async fn get_review_report(
    State(state): State<SharedState>,
    AxumPath(id): AxumPath<i64>,
) -> Result<impl IntoResponse, ApiError> {
    let _ = require_review(&state, id).await?;
    let report = state
        .store
        .run_blocking(move |store| store.get_review_report(id))
        .await?
        .ok_or_else(|| ApiError::not_found(format!("no such review: {id}")))?;
    let value: Option<serde_json::Value> = report
        .report_json
        .as_deref()
        .and_then(|s| serde_json::from_str(s).ok());
    Ok((
        [(header::CACHE_CONTROL, "no-store")],
        Json(value.unwrap_or_else(|| serde_json::json!({ "report": null }))),
    ))
}

/// kb-code-server/review-report/1 (V70-A3X) — the RATIFIED flat schema
/// `PUT /reviews/{id}/report` stores, and the ONLY top-level keys it
/// accepts: `verdict` (a STATE string, e.g. `"pass"`/`"concerns"` — always
/// optional), `verdict_headline`, `verdict_body`, `summary`, `deck`,
/// `risk_score`, `stats`, plus `generated_at` (server-stamped — see
/// [`put_review_report`], accepted-but-always-overwritten so re-PUTting a
/// previous `GET` response never 400s on its own echoed timestamp). Before
/// this ratification, the route stored whatever opaque object it was
/// handed verbatim: the operator's report GENERATOR wrote a NESTED
/// `verdict: {headline, body}`, while the SPA's Report tab always rendered
/// the FLAT `verdict_headline`/`verdict_body` fields — so a
/// generator-authored report silently degraded to "Unset" on read. See
/// [`normalize_report_shape`] for how both shapes are now accepted and
/// folded into this one before storing.
const REPORT_ALLOWED_KEYS: &[&str] = &[
    "verdict",
    "verdict_headline",
    "verdict_body",
    "summary",
    "deck",
    "risk_score",
    "stats",
    "generated_at",
    // A self-describing `"kbc-review-report/1"`-style tag — same pervasive
    // `schema` convention every OTHER wire response in this crate carries
    // (`BranchesResponse::schema`, `PrsResponse::schema`, …), optional
    // here (the route itself never reads it), but a legitimate producer
    // field, not noise to reject.
    "schema",
];

/// 400 problem+json (RFC 7807), `urn:kb:errors:report-shape` (V70-A3X) —
/// mirrors kb-server's own `urn:kb:errors:not-owner` precedent
/// (`kb_server::routes::comments::forbid_if_not_owner`): same shape, same
/// manual `Content-Type` override (axum's `Json` extractor always sets
/// `application/json`, so the problem+json content type has to be applied
/// AFTER `.into_response()`).
fn report_shape_error(detail: impl Into<String>) -> Response {
    let body = serde_json::json!({
        "type": "urn:kb:errors:report-shape",
        "title": "invalid report shape",
        "status": 400,
        "detail": detail.into(),
    });
    let mut resp = (StatusCode::BAD_REQUEST, Json(body)).into_response();
    resp.headers_mut().insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("application/problem+json"),
    );
    resp
}

/// Normalise a `PUT /reviews/{id}/report` body to the [`REPORT_ALLOWED_KEYS`]
/// flat schema, accepting EITHER shape a producer might send — see that
/// const's own doc for the "why both" history:
///
/// - the FLAT shape (`verdict` a plain state STRING, `verdict_headline`/
///   `verdict_body` already top-level) — what the SPA's Report tab renders;
/// - the NESTED shape (`verdict: {headline, body, ...}`) — what the
///   operator's report generator writes.
///
/// A nested `verdict` OBJECT has its `headline`/`body` sub-fields folded
/// into the top-level `verdict_headline`/`verdict_body` keys — WITHOUT
/// overwriting an explicit top-level value the caller ALSO sent (the more
/// specific flat field wins on a genuine conflict) — and the `verdict` key
/// itself is then DROPPED: a nested object is not a valid value for the
/// flat schema's `verdict: string` slot, and there is no "state" to
/// preserve from it. A plain STRING `verdict` (or its total absence)
/// passes through unchanged.
///
/// Returns `Err` (a ready-to-return problem+json [`Response`], `Box`ed per
/// `clippy::result_large_err` — a bare `axum::http::Response<Body>` is
/// 128+ bytes, too large to carry unboxed in a `Result` this crate's own
/// lint gate treats as an error) for: a non-object body, a `verdict` value
/// that's neither a string, an object, nor null, or ANY top-level key
/// outside [`REPORT_ALLOWED_KEYS`] — named explicitly in the error
/// `detail`, never a bare "invalid body". `pub(crate)` (V70-R) — reused
/// verbatim by [`crate::review_findings::compose_review_route`] so
/// `review compose`'s report half is normalised through the exact SAME
/// function `PUT /report` uses, never a second copy of the rules.
pub(crate) fn normalize_report_shape(
    mut body: serde_json::Value,
) -> Result<serde_json::Value, Box<Response>> {
    let Some(obj) = body.as_object_mut() else {
        return Err(Box::new(report_shape_error(
            "report body must be a JSON object",
        )));
    };

    if let Some(verdict) = obj.get("verdict").cloned() {
        match verdict {
            serde_json::Value::Object(nested) => {
                if !obj.contains_key("verdict_headline") {
                    if let Some(h) = nested.get("headline") {
                        obj.insert("verdict_headline".to_string(), h.clone());
                    }
                }
                if !obj.contains_key("verdict_body") {
                    if let Some(b) = nested.get("body") {
                        obj.insert("verdict_body".to_string(), b.clone());
                    }
                }
                obj.remove("verdict");
            }
            serde_json::Value::String(_) | serde_json::Value::Null => {}
            other => {
                return Err(Box::new(report_shape_error(format!(
                    "\"verdict\" must be a string (state) or an object \
                     ({{headline, body}}), got {other}"
                ))));
            }
        }
    }

    if let Some(bad_key) = obj
        .keys()
        .find(|k| !REPORT_ALLOWED_KEYS.contains(&k.as_str()))
    {
        return Err(Box::new(report_shape_error(format!(
            "unknown report field: {bad_key:?}"
        ))));
    }

    Ok(body)
}

/// `PUT /api/reviews/{id}/report` (design doc §2 row 5) — wholesale-replace
/// the report (never a partial merge — same "one owner writes it whole"
/// posture as `pr_meta_json`). `400` when the review has zero patchsets
/// (same rule `put_verdict` enforces). The body is normalised to
/// [`REPORT_ALLOWED_KEYS`]'s flat schema first (V70-A3X —
/// [`normalize_report_shape`]'s own doc has the full contract; an unknown
/// key or malformed `verdict` 400s as `urn:kb:errors:report-shape`
/// problem+json, BEFORE the patchset-existence check or any store write).
/// `generated_at` is server-stamped, overwriting anything the caller sent
/// for that key. The NORMALISED report (not the caller's raw body) is what
/// gets stored and echoed back. LOOPBACK-ONLY. Emits
/// `review.changed{reason:"report"}`.
pub async fn put_review_report(
    State(state): State<SharedState>,
    AxumPath(id): AxumPath<i64>,
    Json(body): Json<serde_json::Value>,
) -> Result<Response, ApiError> {
    let mut body = match normalize_report_shape(body) {
        Ok(b) => b,
        Err(problem) => return Ok(*problem),
    };

    let (review, _, _) = require_review(&state, id).await?;
    state
        .store
        .run_blocking(move |store| -> Result<(), ApiError> {
            store
                .latest_patchset(id)?
                .ok_or_else(|| ApiError::bad_request(format!("review {id} has no patchsets")))?;
            Ok(())
        })
        .await?;
    let now = now_unix();
    body.as_object_mut()
        .expect("normalize_report_shape guarantees an object")
        .insert("generated_at".to_string(), serde_json::json!(now));

    let report_json = serde_json::to_string(&body)
        .map_err(|e| ApiError::new(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    let ok = state
        .store
        .run_blocking(move |store| store.set_review_report(id, &report_json, now))
        .await?;
    if !ok {
        return Err(ApiError::not_found(format!("no such review: {id}")));
    }
    emit_review_changed(&state.bus, id, &review.repo, "report", false);
    Ok(([(header::CACHE_CONTROL, "no-store")], Json(body)).into_response())
}

/// `GET /api/reviews/{id}/artifact` (design doc §2 row 7 / §4.2) — live,
/// UNPERSISTED verification of the review's `artifact_hint_*` against kb.
/// `{hint: null, verified: false}` when no hint is set. Never resolved or
/// cached — kb-sibling/1's "never cache an unreached probe" (invariant
/// #2/#4). Bearer.
pub async fn get_review_artifact(
    State(state): State<SharedState>,
    AxumPath(id): AxumPath<i64>,
) -> Result<impl IntoResponse, ApiError> {
    let _ = require_review(&state, id).await?;
    let binding = state
        .store
        .run_blocking(move |store| store.get_review_pr_binding(id))
        .await?
        .ok_or_else(|| ApiError::not_found(format!("no such review: {id}")))?;
    let (Some(kb), Some(doc_id)) = (
        binding.artifact_hint_kb.as_deref(),
        binding.artifact_hint_id.as_deref(),
    ) else {
        return Ok((
            [(header::CACHE_CONTROL, "no-store")],
            Json(serde_json::json!({ "hint": null, "verified": false })),
        ));
    };

    // A stored hint is NEVER validated at write time (design doc §4.2) — so
    // this read-time verification must degrade honestly on a malformed
    // stored value too, rather than 400ing an otherwise-fine GET request or
    // handing an unvalidated string straight into a URL path.
    if crate::doclens::validate_kb_segment(kb).is_err()
        || crate::doclens::validate_doc_segment(doc_id).is_err()
    {
        return Ok((
            [(header::CACHE_CONTROL, "no-store")],
            Json(serde_json::json!({
                "hint": { "kb": kb, "id": doc_id },
                "verified": false,
                "unavailable_reason": "stored artifact hint fails segment validation",
            })),
        ));
    }

    let body = match state.kb_client.doc_meta(kb, doc_id).await {
        Ok(Some(doc)) => serde_json::json!({
            "hint": { "kb": kb, "id": doc_id },
            "verified": true,
            "doc": {
                "title": doc.title,
                "source_relative": doc.source_relative,
                "kb_tags": doc.kb_tags,
                "kb_category": doc.kb_category,
            },
        }),
        Ok(None) => serde_json::json!({
            "hint": { "kb": kb, "id": doc_id },
            "verified": false,
            "unavailable_reason": "kb reports no such doc",
        }),
        Err(e) => serde_json::json!({
            "hint": { "kb": kb, "id": doc_id },
            "verified": false,
            "unavailable_reason": e.to_string(),
        }),
    };
    Ok(([(header::CACHE_CONTROL, "no-store")], Json(body)))
}

// ── PRR-R4: pr-status probe ──────────────────────────────────────────────
//
// kb v0.39 "The PR Room" (T2), unit R4 — Phase 4 of design-server.md (§2
// row 12) + the R2 read-surface leftover (additive PR-binding/report
// fields on `list_reviews`/`get_review`, above). `GET /reviews/inbox`
// ([`crate::review_inbox`]) and `GET /reviews/{id}/timeline`
// ([`crate::review_timeline`]) are their own new sibling modules — this
// route stays here because it reuses `require_review`/`resolve_commit_sha`/
// `commit_count`, all module-private to this file.

pub const PR_STATUS_SCHEMA: &str = "review-pr-status/1";

/// The LOCAL half of the pr-status probe, pulled out as a pure fn so it is
/// unit-testable without a repo/daemon (`local_matches_pr`, `stale`). See
/// [`pr_status_route`]'s own doc for the full semantics.
fn local_pr_status(
    review_snapshot_head_sha: Option<&str>,
    latest_local_ps_tip_sha: &str,
) -> (bool, bool) {
    let local_matches_pr = review_snapshot_head_sha == Some(latest_local_ps_tip_sha);
    (local_matches_pr, !local_matches_pr)
}

/// `GET /api/reviews/{id}/pr-status` (design doc §2 row 12) — the staleness
/// probe. Bearer.
///
/// The LOCAL half always answers once the review is PR-bound with at least
/// one patchset — it needs no network and cannot itself fail:
/// `review_snapshot_head_sha` is the STORED `pr_head_sha` binding column
/// (whatever GitHub reported at the last successful bind/fetch — set once
/// by `create_review_pr`, refreshable later via `Store::set_review_pr_meta`,
/// a re-fetch route a later phase may add); `latest_local_ps_tip_sha` is
/// this review's current latest captured patchset tip; `local_matches_pr`
/// is that comparison and `stale` is its negation, surfaced as its own
/// named boolean (mirrors `verdict_block`'s own "the comparison is the
/// substance, the boolean is a convenience" shape).
///
/// The LIVE half (`pr_head_sha` — a FRESH `get_pull` call — and
/// `commits_behind`) degrades to `unavailable_reason` on any GitHub-side or
/// origin-resolution failure — same "degrade, don't fail" posture every
/// other `github.rs` read uses (never a 5xx for "GitHub had a bad day").
/// Owner/repo is resolved FRESH via `github::github_repo` each call (the
/// SAME pattern `create_review_pr` uses) rather than parsed back out of the
/// stored `pr_repo_slug` string, which can be a raw origin URL or the
/// literal `"unknown"` (design doc §2 row 1's own fallback) and is not
/// reliably an `owner/name` pair to split on. `commits_behind` additionally
/// requires the freshly reported head to already be resolvable in the
/// LOCAL repo (`git rev-list --count <local ps tip>..<live head>` — the
/// live head must already be a commit this repo has fetched, e.g. via a
/// prior `kb-code pr fetch`); when the live call succeeds but the head
/// isn't locally fetchable yet, `pr_head_sha` is still populated but
/// `commits_behind` stays `null` — a SILENT degrade (no `unavailable_
/// reason`), since the live call itself did succeed; `unavailable_reason`
/// is reserved for describing why the live call (or origin resolution)
/// itself failed.
///
/// `400` when the review is not PR-bound, or has zero patchsets (same
/// "review {id} has no patchsets" rule `put_verdict`/`put_review_report`
/// enforce).
pub async fn pr_status_route(
    State(state): State<SharedState>,
    AxumPath(id): AxumPath<i64>,
) -> Result<impl IntoResponse, ApiError> {
    let (review, repo, _) = require_review(&state, id).await?;
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

    let review_snapshot_head_sha = binding.pr_head_sha.clone();
    let latest_local_ps_tip_sha = latest_ps.tip_sha.clone();
    let (local_matches_pr, stale) = local_pr_status(
        review_snapshot_head_sha.as_deref(),
        &latest_local_ps_tip_sha,
    );

    // LIVE half — see this route's own doc for why owner/repo is resolved
    // fresh rather than parsed from the stored slug.
    let root_for_origin = repo.path.clone();
    let gh_repo_result =
        tokio::task::spawn_blocking(move || crate::github::github_repo(&root_for_origin))
            .await
            .map_err(|e| {
                ApiError::new(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    format!("origin lookup task panicked: {e}"),
                )
            })?;

    let mut pr_head_sha: Option<String> = None;
    let mut commits_behind: Option<i64> = None;
    let mut unavailable_reason: Option<String> = None;

    match gh_repo_result {
        Ok(gh) => {
            match state
                .github
                .get_pull(&gh.owner, &gh.name, pr_number as u64)
                .await
            {
                Ok(pull) => {
                    pr_head_sha = Some(pull.head_sha.clone());
                    let root = repo.path.clone();
                    let head_sha = pull.head_sha.clone();
                    let resolved = tokio::task::spawn_blocking(move || {
                        // A GitHub-reported head sha — caller-adjacent
                        // text, so it goes through the same validator.
                        resolve_commit_sha(&root, &parse_user_ref(&head_sha)?)
                    })
                    .await
                    .map_err(|e| ApiError::new(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
                    if let Ok(resolved_sha) = resolved {
                        let root2 = repo.path.clone();
                        let base = latest_local_ps_tip_sha.clone();
                        let count = tokio::task::spawn_blocking(move || {
                            commit_count(&root2, &base, &resolved_sha)
                        })
                        .await
                        .map_err(|e| {
                            ApiError::new(StatusCode::INTERNAL_SERVER_ERROR, e.to_string())
                        })?;
                        commits_behind = count.ok().map(|n| n as i64);
                    }
                    // `resolved.is_err()` (head not locally fetchable yet):
                    // `commits_behind` stays `None` silently — see the doc.
                }
                Err(e) => unavailable_reason = Some(e.to_string()),
            }
        }
        Err(e) => unavailable_reason = Some(e.to_string()),
    }

    Ok((
        [(header::CACHE_CONTROL, "no-store")],
        Json(serde_json::json!({
            "schema": PR_STATUS_SCHEMA,
            "review_id": id,
            "repo": review.repo,
            "pr_number": pr_number,
            "pr_head_sha": pr_head_sha,
            "review_snapshot_head_sha": review_snapshot_head_sha,
            "latest_local_ps_tip_sha": latest_local_ps_tip_sha,
            "local_matches_pr": local_matches_pr,
            "stale": stale,
            "commits_behind": commits_behind,
            "unavailable_reason": unavailable_reason,
        })),
    ))
}

// --- unit tests ------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn should_auto_capture_requires_all_gates() {
        assert!(should_auto_capture(true, true, false, true));
        assert!(!should_auto_capture(false, true, false, true)); // config off
        assert!(!should_auto_capture(true, false, false, true)); // closed
        assert!(!should_auto_capture(true, true, true, true)); // tip same
        assert!(!should_auto_capture(true, true, false, false)); // debounce
    }

    #[test]
    fn reject_user_ref_blocks_injection_shapes() {
        assert!(reject_user_ref("main").is_ok());
        assert!(reject_user_ref("feature/x").is_ok());
        assert!(reject_user_ref("--output=/tmp/x").is_err());
        assert!(reject_user_ref("-u").is_err());
        assert!(reject_user_ref("").is_err());
        assert!(reject_user_ref("a b").is_err());
        assert!(reject_user_ref("a..b").is_err());
    }

    #[test]
    fn is_full_sha_accepts_only_40_hex() {
        assert!(is_full_sha("0123456789abcdef0123456789abcdef01234567"));
        assert!(!is_full_sha("abc"));
        assert!(!is_full_sha("0123456789abcdef0123456789abcdef0123456g"));
    }

    #[test]
    fn patchset_ref_is_digits_only_namespace() {
        assert_eq!(patchset_ref(3, 2), "refs/kbc/review/3/ps2");
    }

    // --- normalize_report_shape (V70-A3X) -----------------------------

    #[test]
    fn normalize_report_shape_passes_a_flat_body_through_unchanged() {
        let body = serde_json::json!({
            "verdict": "pass",
            "verdict_headline": "Ships it",
            "verdict_body": "Clean diff, no blockers.",
            "summary": "ok",
            "risk_score": 1,
        });
        let out = normalize_report_shape(body.clone()).unwrap();
        assert_eq!(out, body);
    }

    #[test]
    fn normalize_report_shape_folds_a_nested_verdict_object_into_flat_fields() {
        let body = serde_json::json!({
            "verdict": {"headline": "Ships it", "body": "Clean diff, no blockers."},
            "summary": "ok",
        });
        let out = normalize_report_shape(body).unwrap();
        assert!(out.get("verdict").is_none(), "got {out}");
        assert_eq!(out["verdict_headline"], "Ships it");
        assert_eq!(out["verdict_body"], "Clean diff, no blockers.");
        assert_eq!(out["summary"], "ok");
    }

    #[test]
    fn normalize_report_shape_nested_verdict_never_overwrites_an_explicit_flat_field() {
        let body = serde_json::json!({
            "verdict": {"headline": "from nested", "body": "from nested body"},
            "verdict_headline": "explicit wins",
        });
        let out = normalize_report_shape(body).unwrap();
        assert_eq!(out["verdict_headline"], "explicit wins");
        // `verdict_body` had no explicit top-level value, so the nested
        // one still fills it in.
        assert_eq!(out["verdict_body"], "from nested body");
    }

    #[test]
    fn normalize_report_shape_rejects_an_unknown_top_level_key() {
        let body = serde_json::json!({"summary": "ok", "totally_made_up_field": 1});
        let err = normalize_report_shape(body).unwrap_err();
        assert_eq!(err.status(), StatusCode::BAD_REQUEST);
        assert_eq!(
            err.headers().get(header::CONTENT_TYPE).unwrap(),
            "application/problem+json"
        );
    }

    #[test]
    fn normalize_report_shape_rejects_a_verdict_that_is_neither_string_nor_object() {
        let body = serde_json::json!({"verdict": 42});
        assert!(normalize_report_shape(body).is_err());
    }

    #[test]
    fn normalize_report_shape_rejects_a_non_object_body() {
        let err = normalize_report_shape(serde_json::json!("just a string")).unwrap_err();
        assert_eq!(err.status(), StatusCode::BAD_REQUEST);
    }

    #[test]
    fn normalize_report_shape_accepts_a_string_verdict_and_null_verdict_unchanged() {
        let with_string = serde_json::json!({"verdict": "concerns"});
        assert_eq!(
            normalize_report_shape(with_string.clone()).unwrap(),
            with_string
        );
        let with_null = serde_json::json!({"verdict": null, "summary": "x"});
        let out = normalize_report_shape(with_null).unwrap();
        assert_eq!(out["verdict"], serde_json::Value::Null);
    }

    #[test]
    fn verdict_stale_is_false_without_a_verdict_or_newer_ps() {
        let base = ReviewRow {
            id: 1,
            repo: "r".into(),
            title: None,
            base_ref: "main".into(),
            head_ref: "feature".into(),
            session_id: None,
            state: "open".into(),
            created_at: 0,
            updated_at: 0,
            verdict: None,
            verdict_note: None,
            verdict_at: None,
            verdict_ps: None,
        };
        let (v, stale) = verdict_block(&base, Some(2));
        assert!(v.is_null());
        assert!(!stale);

        let set = ReviewRow {
            verdict: Some("approve".into()),
            verdict_ps: Some(1),
            ..base.clone()
        };
        let (v, stale) = verdict_block(&set, Some(2));
        assert_eq!(v["state"], "approve");
        assert!(stale);
        let (_, fresh) = verdict_block(&set, Some(1));
        assert!(!fresh);
        let (_, none) = verdict_block(&set, None);
        assert!(!none);
    }

    #[test]
    fn capture_and_gc_on_a_real_repo() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path();
        fn git(dir: &Path, args: &[&str]) {
            let out = Command::new("git")
                .arg("-C")
                .arg(dir)
                .args(args)
                .output()
                .unwrap();
            assert!(
                out.status.success(),
                "{}",
                String::from_utf8_lossy(&out.stderr)
            );
        }
        git(dir, &["init", "-q", "-b", "main"]);
        git(dir, &["config", "user.email", "t@e.com"]);
        git(dir, &["config", "user.name", "T"]);
        std::fs::write(dir.join("a.txt"), "one\n").unwrap();
        git(dir, &["add", "-A"]);
        git(dir, &["commit", "-q", "-m", "c1"]);
        git(dir, &["checkout", "-q", "-b", "feature"]);
        std::fs::write(dir.join("a.txt"), "two\n").unwrap();
        git(dir, &["add", "-A"]);
        git(dir, &["commit", "-q", "-m", "c2"]);

        let db = tempfile::tempdir().unwrap();
        let store = Store::open(&db.path().join("i.db")).unwrap();
        let bus = EventBus::default();
        let id = store
            .create_review("r", Some("t"), "main", "feature", None, 1)
            .unwrap();
        let review = store.get_review(id).unwrap().unwrap();
        let ps1 = capture_patchset(&store, &bus, dir, &review, 50, false).unwrap();
        assert_eq!(ps1.ps_number, 1);
        // ref exists
        let show = Command::new("git")
            .arg("-C")
            .arg(dir)
            .args(["show-ref", "--verify", &patchset_ref(id, 1)])
            .output()
            .unwrap();
        assert!(show.status.success());

        // amend → ps2
        std::fs::write(dir.join("a.txt"), "three\n").unwrap();
        git(dir, &["add", "-A"]);
        git(dir, &["commit", "-q", "--amend", "-m", "c2b"]);
        let ps2 = capture_patchset(&store, &bus, dir, &review, 50, false).unwrap();
        assert_eq!(ps2.ps_number, 2);
        assert_ne!(ps1.tip_sha, ps2.tip_sha);

        // max_patchsets=1 → next capture GCs ps1
        std::fs::write(dir.join("a.txt"), "four\n").unwrap();
        git(dir, &["add", "-A"]);
        git(dir, &["commit", "-q", "--amend", "-m", "c2c"]);
        let ps3 = capture_patchset(&store, &bus, dir, &review, 1, false).unwrap();
        assert_eq!(ps3.ps_number, 3);
        assert!(store.get_patchset(id, 1).unwrap().is_none());
        let show1 = Command::new("git")
            .arg("-C")
            .arg(dir)
            .args(["show-ref", "--verify", &patchset_ref(id, 1)])
            .output()
            .unwrap();
        assert!(!show1.status.success(), "ps1 ref must be gone after GC");
    }

    // -- PRR-R4: pr-status local half + additive read fields -------------

    #[test]
    fn local_pr_status_matches_when_snapshot_equals_latest_ps_tip() {
        let (matches, stale) = local_pr_status(Some("abc123"), "abc123");
        assert!(matches);
        assert!(!stale);
    }

    #[test]
    fn local_pr_status_is_stale_when_ps_tip_diverges_from_the_snapshot() {
        let (matches, stale) = local_pr_status(Some("abc123"), "def456");
        assert!(!matches);
        assert!(stale);
    }

    #[test]
    fn local_pr_status_is_stale_when_there_is_no_stored_snapshot_at_all() {
        // Should be unreachable in practice (`create_review_pr` always
        // stamps `pr_head_sha`), but a missing snapshot must never read as
        // a false "matches" — same "an unknown never reads as fine"
        // discipline as the rest of this crate's degrade posture.
        let (matches, stale) = local_pr_status(None, "def456");
        assert!(!matches);
        assert!(stale);
    }

    #[test]
    fn pr_binding_and_report_fields_are_additive_and_parse_cheaply() {
        let binding = ReviewPrBinding {
            pr_number: Some(7),
            pr_repo_slug: Some("acme/widget".into()),
            pr_head_sha: Some("deadbeef".into()),
            pr_meta_json: Some(r#"{"title":"x","draft":false}"#.into()),
            pr_meta_fetched_at: Some(1_000),
            artifact_hint_kb: Some("platform".into()),
            artifact_hint_id: Some("doc123".into()),
        };
        let report = ReviewReport {
            report_json: Some(r#"{"summary":"ok","risk_score":0.42}"#.into()),
            report_updated_at: Some(2_000),
        };
        let fields = pr_binding_and_report_fields(&binding, &report);
        assert_eq!(fields["pr_number"], 7);
        assert_eq!(fields["pr_repo_slug"], "acme/widget");
        assert_eq!(fields["pr_head_sha"], "deadbeef");
        assert_eq!(fields["pr_meta"]["title"], "x");
        assert_eq!(fields["artifact_hint_kb"], "platform");
        assert_eq!(fields["artifact_hint_id"], "doc123");
        assert_eq!(fields["has_report"], true);
        assert_eq!(fields["report_risk_score"], 0.42);
    }

    #[test]
    fn pr_binding_and_report_fields_degrade_honestly_when_unbound_and_unreported() {
        let binding = ReviewPrBinding::default();
        let report = ReviewReport::default();
        let fields = pr_binding_and_report_fields(&binding, &report);
        assert!(fields["pr_number"].is_null());
        assert!(fields["pr_meta"].is_null());
        assert_eq!(fields["has_report"], false);
        assert!(fields["report_risk_score"].is_null());
    }

    #[test]
    fn merge_pr_binding_and_report_fields_extends_an_existing_object_in_place() {
        let mut row = serde_json::json!({ "id": 1, "repo": "r" });
        let binding = ReviewPrBinding {
            pr_number: Some(9),
            ..ReviewPrBinding::default()
        };
        let report = ReviewReport::default();
        merge_pr_binding_and_report_fields(&mut row, &binding, &report);
        // Existing keys survive untouched; new keys are additive.
        assert_eq!(row["id"], 1);
        assert_eq!(row["repo"], "r");
        assert_eq!(row["pr_number"], 9);
        assert_eq!(row["has_report"], false);
    }
}
