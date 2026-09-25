//! RS-U10b — `review sync` and `review status` for agent reviewers
//! (README §13, D13/D15/D18; §10 "what sync replaces").
//!
//! # `POST /api/reviews/sync` (loopback-only; `?async=1` = a daemon job)
//!
//! ONE idempotent daemon operation per PR, replacing the morning skill's
//! hand-written four-branch provisioning (does a review exist? is the PR
//! merged? has the head moved? was it rebased?):
//!
//! 1. read the PR from the forge API (`GET /repos/{o}/{r}/pulls/{n}`,
//!    through the same credential ladder start-pr uses — file/env token,
//!    the daemon's own `gh-cli` login for a ready store, the caller's
//!    loopback-only `gh_token`);
//! 2. a MERGED PR with a review is FINAL: nothing is fetched or captured
//!    (`reason: merged-final`). A closed-unmerged PR with a review is
//!    likewise left alone (`reason: unchanged` + a `pr-closed` warning);
//! 3. otherwise it runs [`reviews::create_review_pr_value`] — the SAME
//!    create-or-reuse start-pr runs — which creates the review if missing
//!    (the base chain + one fetch of base and head), or re-fetches base +
//!    head into the store and snapshots ONLY when the `(tip, merge-base)`
//!    pair changed (D13), following a PR retarget when `set_by=auto`
//!    (D15). A closed review whose PR is open again is reopened.
//!
//! The answer (`kbc-review-sync/1`, see [`SYNC_SCHEMA`]) names WHY:
//! `created | head-moved | base-moved | retargeted | unchanged |
//! merged-final` ([`SyncReason`]). The base branch merely advancing changes
//! nothing (README §3, "as on GitHub"): the merge-base of an un-rebased
//! head stays put, so that is `unchanged`, never `base-moved` —
//! `base-moved` is reserved for a MINTED patchset whose merge-base moved
//! under the same tip (a rewritten/re-pointed base). `dry_run` computes
//! the reason from the forge answer and the stored rows without fetching
//! or writing anything (it cannot predict `base-moved`, which needs a
//! fetch).
//!
//! `open: true` is the whole morning loop: list the forge's open PRs (and,
//! with `merged_since`, the PRs merged since then), and run the single
//! sync for each, SEQUENTIALLY, each under the per-repo sync lock — so a
//! caller never needs a "the lead owns every fetch" rule. A per-item
//! failure is reported IN LINE (`ok: false` + a typed error); the listing
//! itself failing is the whole call's error.
//!
//! # `GET /api/reviews/{id}/status` (bearer; `?fetch=1` loopback-only)
//!
//! Read-only: `head_moved` compares the forge's (or, with `?fetch=1`, the
//! freshly store-fetched) PR head with the LATEST PATCHSET TIP — never the
//! `pr_head_sha` snapshot, which is what the morning skill used to read
//! wrongly. Plus the base block, the file-count drift against GitHub's
//! own `changed_files`, verdict staleness and the open-finding count.
//! `?fetch=1` fetches base + PR head INTO THE REVIEW STORE only (a ready
//! store; the user clone is never written) and writes no row.
//!
//! Security posture (crate CLAUDE.md): this module spawns nothing. Git
//! runs through `StoreCtx` (the hardened `StoreGit`) and `reviews`'
//! helpers; the forge through `GithubClient` (tokens resolved per request,
//! never logged, never echoed — the caller's `gh_token` is admitted by the
//! same loopback-only gate start-pr uses and never appears in a job).

use crate::config::RepoEntry;
use crate::entities::RouteContract;
use crate::git::roots::GitCtx;
use crate::github::{GithubApiError, GithubClient, GithubRepo, PrSyncOut};
use crate::review_base::capture::pr_of_head;
use crate::review_base::{warn, warning, BaseWarningOut};
use crate::review_jobs::JobHandle;
use crate::reviews::{self, CreateReviewPrBody, OnClosed, StartPrParams, ERR_REVIEW_CLOSED};
use crate::routes::{find_repo, ApiError};
use crate::state::SharedState;
use crate::store::{ReviewPatchsetRow, ReviewRow, StoreBlocking};
use axum::extract::{Path as AxumPath, Query, State};
use axum::http::{header, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, OnceLock};

#[cfg(test)]
mod tests;

pub const SYNC_SCHEMA: &str = "kbc-review-sync/1";
pub const SYNC_OPEN_SCHEMA: &str = "kbc-review-sync-open/1";
pub const STATUS_SCHEMA: &str = "kbc-review-status/1";

/// The forge API could not answer (not a GitHub origin, no credentials,
/// rate-limited, offline). A per-PR sync degrades with a warning; the
/// `open` listing refuses with this URN (502).
pub const URN_FORGE_UNAVAILABLE: &str = "urn:kb:errors:forge-unavailable";
/// `GET …/status?fetch=1` from a non-loopback caller (403).
pub const URN_FETCH_LOOPBACK_ONLY: &str = "urn:kb:errors:fetch-loopback-only";

/// Warning codes this module adds (beside the base model's own, which pass
/// through verbatim).
pub mod sync_warn {
    pub const FORGE_UNAVAILABLE: &str = "forge-unavailable";
    pub const BASE_IGNORED: &str = "base-ignored";
    pub const PR_CLOSED: &str = "pr-closed";
    pub const WOULD_REOPEN: &str = "would-reopen";
    pub const FETCH_UNAVAILABLE: &str = "fetch-unavailable";
    pub const FETCH_FAILED: &str = "fetch-failed";
}

// --- the reason vocabulary -------------------------------------------------------

/// Why a sync did (or did not) mint a patchset. Closed set, README §13.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SyncReason {
    Created,
    HeadMoved,
    BaseMoved,
    Retargeted,
    Unchanged,
    MergedFinal,
}

impl SyncReason {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Created => "created",
            Self::HeadMoved => "head-moved",
            Self::BaseMoved => "base-moved",
            Self::Retargeted => "retargeted",
            Self::Unchanged => "unchanged",
            Self::MergedFinal => "merged-final",
        }
    }

    /// A re-capture's answer as a reason. Pure. A retarget the review
    /// FOLLOWED is `retargeted` even when the new target's merge-base left
    /// the pair unchanged (nothing minted, but the policy moved); otherwise
    /// an unminted capture is `unchanged`, and a minted one says why by its
    /// patchset `kind` (`push`/`rebase` → the head moved; `base-moved`/
    /// `base-corrected` → only the merge-base moved; `retarget` — also a
    /// vanished `auto` base re-resolved — → retargeted; `forced`, a re-mint
    /// of an IDENTICAL pair that sync itself never asks for, → unchanged:
    /// nothing moved). `kind` is read ONLY when `minted` — an unminted
    /// capture reports the LATEST patchset's old kind.
    pub fn from_capture(minted: bool, kind: Option<&str>, retargeted: bool) -> Self {
        if retargeted || (minted && kind == Some("retarget")) {
            return Self::Retargeted;
        }
        if !minted {
            return Self::Unchanged;
        }
        match kind {
            Some("base-moved") | Some("base-corrected") => Self::BaseMoved,
            Some("forced") => Self::Unchanged,
            _ => Self::HeadMoved,
        }
    }

    /// `dry_run`'s prediction from stored rows + the forge answer. Pure.
    /// A retarget is predicted only where the daemon would follow it
    /// (`track` + `set_by=auto`); `base-moved` is never predicted (it needs
    /// a fetch).
    pub fn predict(
        latest_tip: Option<&str>,
        forge_head: Option<&str>,
        mode: Option<&str>,
        branch: Option<&str>,
        set_by: &str,
        forge_base: Option<&str>,
    ) -> Self {
        if let (Some("track"), Some(b), "auto", Some(fb)) = (mode, branch, set_by, forge_base) {
            if b != fb {
                return Self::Retargeted;
            }
        }
        match (latest_tip, forge_head) {
            (Some(t), Some(h)) if t != h => Self::HeadMoved,
            (None, Some(_)) => Self::HeadMoved,
            _ => Self::Unchanged,
        }
    }
}

/// The reason for a create-or-reuse answer (`create_review_pr_value`'s
/// `(status, body)`). Pure.
pub fn reason_from_envelope(status: StatusCode, body: &Value) -> SyncReason {
    if status == StatusCode::CREATED {
        return SyncReason::Created;
    }
    let minted = body["minted"].as_bool().unwrap_or(false);
    let retargeted = body["warnings"]
        .as_array()
        .is_some_and(|ws| ws.iter().any(|w| w["code"] == warn::RETARGETED));
    SyncReason::from_capture(minted, body["kind"].as_str(), retargeted)
}

// --- the forge block ---------------------------------------------------------------

/// `forge{…}` on sync/status: what the forge API said about the PR.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct ForgeOut {
    /// `false` = the API could not answer; see `unavailable`.
    pub available: bool,
    /// `open` | `closed` | `merged`.
    pub state: Option<String>,
    pub changed_files: Option<u64>,
    pub title: Option<String>,
    /// The PR's target branch (`base.ref`).
    pub base_ref: Option<String>,
    pub head_sha: Option<String>,
    pub merged_at: Option<String>,
    /// Why the API did not answer: `not-github`, `no-credentials`,
    /// `not-found`, `forbidden`, `rate-limited`, `network`.
    pub unavailable: Option<String>,
}

impl ForgeOut {
    pub fn from_pr(p: &PrSyncOut) -> Self {
        let state = if p.merged {
            "merged".to_string()
        } else {
            p.state.clone()
        };
        Self {
            available: true,
            state: Some(state),
            changed_files: p.changed_files,
            title: Some(p.title.clone()),
            base_ref: Some(p.base_ref.clone()),
            head_sha: Some(p.head_sha.clone()),
            merged_at: p.merged_at.clone(),
            unavailable: None,
        }
    }

    pub fn unavailable(code: &str) -> Self {
        Self {
            unavailable: Some(code.to_string()),
            ..Self::default()
        }
    }

    pub fn is_merged(&self) -> bool {
        self.state.as_deref() == Some("merged")
    }

    pub fn is_closed_unmerged(&self) -> bool {
        self.state.as_deref() == Some("closed")
    }
}

fn unavailable_code(e: &GithubApiError, had_credentials: bool) -> String {
    let u = e.to_pr_meta_unavailable(had_credentials);
    serde_json::to_value(u.code)
        .ok()
        .and_then(|v| v.as_str().map(str::to_string))
        .unwrap_or_else(|| "network".to_string())
}

/// The GitHub `owner/name` the PR lives under, a client carrying the
/// request's credential ladder (file/env > the store's `gh-cli` login >
/// the caller's `gh_token`), and any D12 warning
/// (`credential-account-mismatch`). With a ready review store the project
/// is the STORE's `forge_slug` — never the member's `origin`, which may be
/// a fork (RS-U6); without one it is the member's `origin` (today's
/// work-tree behaviour, the same answer start-pr's own enrichment uses).
async fn forge_client(
    state: &SharedState,
    repo: &RepoEntry,
    gh_token: Option<String>,
) -> (Option<GithubRepo>, GithubClient, Vec<BaseWarningOut>) {
    let github = state.github.with_cli_token(gh_token);
    let st = state.clone();
    let name = repo.name.clone();
    let store = tokio::task::spawn_blocking(move || {
        let handle = st.review_stores.handle_for_repo(&st.store, &name).ok()?;
        let row = st.store.get_review_store(handle.id).ok().flatten();
        Some((handle, row))
    })
    .await
    .ok()
    .flatten();
    match store {
        Some((handle, row)) => {
            let gh = row.as_ref().and_then(reviews::store_github_repo);
            let (github, warnings) = reviews::github_with_gh_cli_warned(
                state,
                github,
                &handle,
                &repo.name,
                crate::review_store::GhCli::from_process_env(),
            )
            .await;
            (gh, github, warnings)
        }
        None => {
            let root = repo.path.clone();
            let gh = tokio::task::spawn_blocking(move || crate::github::github_repo(&root))
                .await
                .ok()
                .and_then(Result::ok);
            (gh, github, vec![])
        }
    }
}

/// The forge's answer for PR `pr` (+ credential warnings) — never an
/// error: an API that cannot answer is `available: false` + the reason.
async fn forge_pr(
    state: &SharedState,
    repo: &RepoEntry,
    pr: u32,
    gh_token: Option<String>,
) -> (ForgeOut, Vec<BaseWarningOut>) {
    let (gh, client, warnings) = forge_client(state, repo, gh_token).await;
    let Some(gh) = gh else {
        return (ForgeOut::unavailable("not-github"), warnings);
    };
    let had = client.has_credentials();
    let forge = match client
        .get_pull_sync(&gh.owner, &gh.name, u64::from(pr))
        .await
    {
        Ok(p) => ForgeOut::from_pr(&p),
        Err(e) => ForgeOut::unavailable(&unavailable_code(&e, had)),
    };
    (forge, warnings)
}

fn forge_warning(pr: u32, forge: &ForgeOut) -> BaseWarningOut {
    warning(
        sync_warn::FORGE_UNAVAILABLE,
        format!(
            "the forge API could not answer for PR #{pr} ({}); merged/retarget state and the GitHub file count are unknown",
            forge.unavailable.as_deref().unwrap_or("unavailable")
        ),
    )
}

// --- dates --------------------------------------------------------------------------

/// `merged_since`: `YYYY-MM-DD` (00:00 UTC) or RFC 3339 → unix seconds.
pub fn parse_since(s: &str) -> Result<i64, String> {
    let t = s.trim();
    if let Ok(dt) = chrono::DateTime::parse_from_rfc3339(t) {
        return Ok(dt.timestamp());
    }
    if let Ok(d) = chrono::NaiveDate::parse_from_str(t, "%Y-%m-%d") {
        if let Some(dt) = d.and_hms_opt(0, 0, 0) {
            return Ok(dt.and_utc().timestamp());
        }
    }
    Err(format!(
        "merged_since must be YYYY-MM-DD or an RFC 3339 timestamp, got {s:?}"
    ))
}

/// Was a PR merged at or after `since`? `merged_at` is GitHub's RFC 3339.
pub fn merged_on_or_after(merged_at: Option<&str>, since: i64) -> bool {
    merged_at
        .and_then(|m| chrono::DateTime::parse_from_rfc3339(m).ok())
        .is_some_and(|d| d.timestamp() >= since)
}

// --- the per-repo sync lock ------------------------------------------------------------

/// One async lock per member clone: syncs of one repo run one at a time
/// (the store's own fetch/ops locks still serialize the git work inside).
/// A `tokio` mutex (its guard crosses awaits by design); the map guard is
/// `parking_lot` and never crosses one.
fn sync_lock(root: &Path) -> Arc<tokio::sync::Mutex<()>> {
    type Locks = parking_lot::Mutex<HashMap<PathBuf, Arc<tokio::sync::Mutex<()>>>>;
    static LOCKS: OnceLock<Locks> = OnceLock::new();
    let map = LOCKS.get_or_init(Default::default);
    map.lock().entry(root.to_path_buf()).or_default().clone()
}

// --- one PR ---------------------------------------------------------------------------

/// One `review sync --pr N`.
pub struct SyncRequest {
    pub repo: String,
    pub pr: u32,
    pub title: Option<String>,
    /// `--base`: applies on CREATION only (an existing review's base moves
    /// with `review retrack`).
    pub base: Option<String>,
    pub dry_run: bool,
    pub gh_token: Option<String>,
}

fn warnings_json(ws: &[BaseWarningOut]) -> Vec<Value> {
    ws.iter().map(|w| json!(w)).collect()
}

/// Sync one PR. See the module doc.
pub(crate) async fn sync_one(
    state: &SharedState,
    req: SyncRequest,
    job: Option<JobHandle>,
) -> Result<Value, ApiError> {
    let (repo, _) = find_repo(state, &req.repo)?;
    let repo = repo.clone();
    let lock = sync_lock(&repo.path);
    let _serial = lock.lock().await;

    crate::review_jobs::set_stage(&job, "forge");
    let (forge, mut warnings) = forge_pr(state, &repo, req.pr, req.gh_token.clone()).await;
    if !forge.available {
        warnings.push(forge_warning(req.pr, &forge));
    }
    let name = req.repo.clone();
    let prn = i64::from(req.pr);
    let existing = state
        .store
        .run_blocking(move |store| store.get_review_by_pr_binding(&name, prn))
        .await?;
    if let (Some(ex), Some(b)) = (&existing, &req.base) {
        warnings.push(warning(
            sync_warn::BASE_IGNORED,
            format!(
                "review {} already exists; --base {b:?} applies only when a review is created — use `kb-code review retrack {}` to change its base",
                ex.id, ex.id
            ),
        ));
    }

    // A merged (or closed-unmerged) PR with a review is final.
    if let Some(ex) = &existing {
        if forge.is_merged() || forge.is_closed_unmerged() {
            let reason = if forge.is_merged() {
                SyncReason::MergedFinal
            } else {
                warnings.push(warning(
                    sync_warn::PR_CLOSED,
                    format!(
                        "PR #{} is closed without merging; nothing was fetched or captured",
                        req.pr
                    ),
                ));
                SyncReason::Unchanged
            };
            return finish(
                state,
                &repo,
                Outcome {
                    review_id: ex.id,
                    pr: req.pr,
                    created: false,
                    minted: false,
                    reason,
                    base: None,
                    dry_run: req.dry_run,
                },
                forge,
                warnings_json(&warnings),
            )
            .await;
        }
    }

    if req.dry_run {
        return dry_run(state, &repo, &req, existing.as_ref(), forge, warnings).await;
    }

    let on_closed = match &existing {
        Some(ex) if ex.state != "open" => {
            if forge.state.as_deref() == Some("open") {
                Some(OnClosed::Reopen)
            } else {
                return Err(ApiError::new(
                    StatusCode::CONFLICT,
                    format!(
                        "review {} for PR #{} is closed and the forge cannot confirm the PR is open — reopen it with `kb-code review start-pr --repo {} --pr {} --reopen`",
                        ex.id, req.pr, req.repo, req.pr
                    ),
                )
                .with_problem_type(ERR_REVIEW_CLOSED));
            }
        }
        _ => None,
    };

    crate::review_jobs::set_stage(&job, "fetch");
    let body = CreateReviewPrBody {
        repo: req.repo.clone(),
        pr_number: req.pr,
        base_ref: if existing.is_none() {
            req.base.clone()
        } else {
            None
        },
        title: req.title.clone().or_else(|| forge.title.clone()),
        session_id: None,
        // The chain's `caller` rung: the target the forge answered with
        // (the daemon's own forge-API rung, when it resolves, wins anyway).
        caller_base_ref: forge.base_ref.clone(),
        gh_token: req.gh_token.clone(),
    };
    let (status, value) = reviews::create_review_pr_value(state, body, job, on_closed).await?;
    if !status.is_success() {
        let mut e = ApiError::new(
            status,
            value["error"]
                .as_str()
                .unwrap_or("review sync failed")
                .to_string(),
        );
        if value["type"].as_str() == Some(ERR_REVIEW_CLOSED) {
            e = e.with_problem_type(ERR_REVIEW_CLOSED);
        }
        return Err(e);
    }
    let review_id = value["id"].as_i64().ok_or_else(|| {
        ApiError::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            "start-pr answered without a review id",
        )
    })?;
    let reason = reason_from_envelope(status, &value);
    let mut all = warnings_json(&warnings);
    if let Some(ws) = value["warnings"].as_array() {
        all.extend(ws.iter().cloned());
    }
    let minted = value["minted"]
        .as_bool()
        .unwrap_or(status == StatusCode::CREATED);
    finish(
        state,
        &repo,
        Outcome {
            review_id,
            pr: req.pr,
            created: status == StatusCode::CREATED,
            minted,
            reason,
            base: Some(value["base"].clone()),
            dry_run: false,
        },
        forge,
        all,
    )
    .await
}

/// `dry_run`: predict from the stored rows + the forge answer; writes and
/// fetches nothing.
async fn dry_run(
    state: &SharedState,
    repo: &RepoEntry,
    req: &SyncRequest,
    existing: Option<&ReviewRow>,
    forge: ForgeOut,
    mut warnings: Vec<BaseWarningOut>,
) -> Result<Value, ApiError> {
    let Some(ex) = existing else {
        return Ok(json!({
            "schema": SYNC_SCHEMA,
            "repo": repo.name,
            "pr_number": req.pr,
            "review_id": Value::Null,
            "review_state": Value::Null,
            "created": true,
            "ps": Value::Null,
            "minted": true,
            "reason": SyncReason::Created.as_str(),
            "kind": Value::Null,
            "dry_run": true,
            "base": Value::Null,
            "head_sha": forge.head_sha,
            "files_count": Value::Null,
            "files_equal": Value::Null,
            "forge": forge,
            "verdict": Value::Null,
            "warnings": warnings_json(&warnings),
        }));
    };
    if ex.state != "open" {
        warnings.push(warning(
            sync_warn::WOULD_REOPEN,
            format!("review {} is {}; a real sync reopens it", ex.id, ex.state),
        ));
    }
    let root = repo.path.clone();
    let review = ex.clone();
    let (latest_tip, base) = state
        .store
        .run_blocking(move |store| -> Result<_, ApiError> {
            let latest = store.latest_patchset(review.id)?;
            let (base, _) = reviews::review_base_block(
                store,
                &review,
                &root,
                latest.as_ref().map(|p| p.base_sha.as_str()),
            );
            Ok((latest.map(|p| p.tip_sha), base))
        })
        .await?;
    let reason = SyncReason::predict(
        latest_tip.as_deref(),
        forge.head_sha.as_deref(),
        base.mode.as_deref(),
        base.branch.as_deref(),
        &base.set_by,
        forge.base_ref.as_deref(),
    );
    finish(
        state,
        repo,
        Outcome {
            review_id: ex.id,
            pr: req.pr,
            created: false,
            minted: reason != SyncReason::Unchanged,
            reason,
            base: None,
            dry_run: true,
        },
        forge,
        warnings_json(&warnings),
    )
    .await
}

struct Outcome {
    review_id: i64,
    pr: u32,
    created: bool,
    minted: bool,
    reason: SyncReason,
    /// The capture's own `base{…}` block, when a capture ran.
    base: Option<Value>,
    dry_run: bool,
}

/// Compose the `kbc-review-sync/1` body from the stored rows.
async fn finish(
    state: &SharedState,
    repo: &RepoEntry,
    o: Outcome,
    forge: ForgeOut,
    mut warnings: Vec<Value>,
) -> Result<Value, ApiError> {
    let root = repo.path.clone();
    let id = o.review_id;
    let (review, latest, kind, base_block, block_warnings) = state
        .store
        .run_blocking(move |store| -> Result<_, ApiError> {
            let review = store
                .get_review(id)?
                .ok_or_else(|| ApiError::not_found(format!("no such review: {id}")))?;
            let latest = store.latest_patchset(id)?;
            let kind = match &latest {
                Some(p) => store
                    .get_patchset_base(id, p.ps_number)?
                    .and_then(|b| b.kind),
                None => None,
            };
            let (b, w) = reviews::review_base_block(
                store,
                &review,
                &root,
                latest.as_ref().map(|p| p.base_sha.as_str()),
            );
            Ok((review, latest, kind, b, w))
        })
        .await?;
    let base = match o.base.filter(Value::is_object) {
        Some(b) => b,
        None => {
            warnings.extend(warnings_json(&block_warnings));
            json!(base_block)
        }
    };
    let files_count = count_files(state, repo, latest.as_ref()).await;
    let files_equal = match (files_count, forge.changed_files) {
        (Some(a), Some(b)) => Some(a as u64 == b),
        _ => None,
    };
    Ok(json!({
        "schema": SYNC_SCHEMA,
        "repo": review.repo,
        "pr_number": o.pr,
        "review_id": review.id,
        "review_state": review.state,
        "created": o.created,
        "ps": latest.as_ref().map(|p| p.ps_number),
        "minted": o.minted,
        "reason": o.reason.as_str(),
        "kind": kind,
        "dry_run": o.dry_run,
        "base": base,
        "head_sha": latest.as_ref().map(|p| p.tip_sha.clone()),
        "files_count": files_count,
        "files_equal": files_equal,
        "forge": forge,
        "verdict": { "state": review.verdict, "ps": review.verdict_ps },
        "warnings": warnings,
    }))
}

/// The latest patchset's changed-file count against its OWN base (`-M`:
/// a rename is one file, as on GitHub). `None` when it cannot be computed.
async fn count_files(
    state: &SharedState,
    repo: &RepoEntry,
    latest: Option<&ReviewPatchsetRow>,
) -> Option<usize> {
    let ps = latest?;
    let ctx = GitCtx::resolve_entry(&state.store, repo).await;
    let (b, t) = (ps.base_sha.clone(), ps.tip_sha.clone());
    tokio::task::spawn_blocking(move || reviews::files_changed(&ctx, &b, &t))
        .await
        .ok()
        .and_then(Result::ok)
        .map(|f| f.len())
}

// --- the morning loop -------------------------------------------------------------------

fn error_code(e: &ApiError) -> String {
    match e.problem_type() {
        Some(urn) => urn.to_string(),
        None => format!(
            "urn:kb:errors:{}",
            match e.status_code().as_u16() {
                400 => "bad-request",
                404 => "not-found",
                409 => "conflict",
                502 => "upstream",
                503 => "unavailable",
                _ => "daemon-error",
            }
        ),
    }
}

/// `review sync --open [--merged-since DATE]`. See the module doc.
pub(crate) async fn sync_open(
    state: &SharedState,
    repo_name: &str,
    merged_since: Option<String>,
    dry_run: bool,
    gh_token: Option<String>,
    job: Option<JobHandle>,
) -> Result<Value, ApiError> {
    let (repo, _) = find_repo(state, repo_name)?;
    let repo = repo.clone();
    let since = merged_since
        .as_deref()
        .map(parse_since)
        .transpose()
        .map_err(ApiError::bad_request)?;
    crate::review_jobs::set_stage(&job, "list");
    let (gh, client, list_warnings) = forge_client(state, &repo, gh_token.clone()).await;
    let Some(gh) = gh else {
        return Err(ApiError::new(
            StatusCode::BAD_GATEWAY,
            format!(
                "repo {} has no GitHub origin; `review sync --open` needs the forge API to list PRs — sync PRs one by one with --pr",
                repo.name
            ),
        )
        .with_problem_type(URN_FORGE_UNAVAILABLE));
    };
    let had = client.has_credentials();
    let listing_err = |e: GithubApiError| {
        let u = e.to_pr_meta_unavailable(had);
        ApiError::new(
            StatusCode::BAD_GATEWAY,
            format!("the forge API could not list PRs: {}", u.hint),
        )
        .with_problem_type(URN_FORGE_UNAVAILABLE)
    };
    let (open, mut truncated) = client
        .list_pulls_sync(&gh.owner, &gh.name, "open")
        .await
        .map_err(listing_err)?;
    let mut prs: Vec<(u32, &'static str)> = open
        .iter()
        .filter_map(|p| u32::try_from(p.number).ok().map(|n| (n, "open")))
        .collect();
    if let Some(since) = since {
        let (closed, t) = client
            .list_pulls_sync(&gh.owner, &gh.name, "closed")
            .await
            .map_err(listing_err)?;
        truncated |= t;
        prs.extend(
            closed
                .iter()
                .filter(|p| p.merged && merged_on_or_after(p.merged_at.as_deref(), since))
                .filter_map(|p| u32::try_from(p.number).ok().map(|n| (n, "merged"))),
        );
    }
    prs.sort_by_key(|(n, _)| *n);
    prs.dedup_by_key(|(n, _)| *n);

    crate::review_jobs::set_stage(&job, "sync");
    let mut items = Vec::with_capacity(prs.len());
    let mut failed = 0usize;
    for (n, listed) in prs {
        let req = SyncRequest {
            repo: repo.name.clone(),
            pr: n,
            title: None,
            base: None,
            dry_run,
            gh_token: gh_token.clone(),
        };
        match sync_one(state, req, None).await {
            Ok(mut v) => {
                v["ok"] = json!(true);
                v["listed_as"] = json!(listed);
                items.push(v);
            }
            Err(e) => {
                failed += 1;
                items.push(json!({
                    "ok": false,
                    "pr_number": n,
                    "listed_as": listed,
                    "error": {
                        "code": error_code(&e),
                        "message": e.message(),
                        "status": e.status_code().as_u16(),
                    },
                }));
            }
        }
    }
    Ok(json!({
        "schema": SYNC_OPEN_SCHEMA,
        "repo": repo.name,
        "merged_since": merged_since,
        "dry_run": dry_run,
        "count": items.len(),
        "failed": failed,
        "truncated": truncated,
        "items": items,
        "warnings": warnings_json(&list_warnings),
    }))
}

// --- POST /api/reviews/sync -----------------------------------------------------------------

/// `POST /api/reviews/sync` body. Exactly one of `pr_number` / `open`.
/// No `Debug`: `gh_token` must never reach a log line.
#[derive(Deserialize)]
pub struct SyncBody {
    pub repo: String,
    #[serde(default)]
    pub pr_number: Option<u32>,
    #[serde(default)]
    pub open: bool,
    #[serde(default)]
    pub merged_since: Option<String>,
    #[serde(default)]
    pub title: Option<String>,
    /// `--base` grammar; creation only.
    #[serde(default)]
    pub base_ref: Option<String>,
    #[serde(default)]
    pub dry_run: bool,
    /// The CLI's `--gh-token-from-cli` value (loopback-only, never
    /// persisted — the start-pr rule).
    #[serde(default)]
    pub gh_token: Option<String>,
}

/// Validate the body's shape. Pure.
pub fn validate_sync_body(b: &SyncBody) -> Result<(), String> {
    match (b.pr_number, b.open) {
        (Some(_), true) => return Err("pass pr_number OR open:true, not both".into()),
        (None, false) => return Err("pass pr_number (one PR) or open:true (every open PR)".into()),
        _ => {}
    }
    if b.merged_since.is_some() && !b.open {
        return Err("merged_since applies only with open:true".into());
    }
    if let Some(s) = &b.merged_since {
        parse_since(s)?;
    }
    if b.open && (b.title.is_some() || b.base_ref.is_some()) {
        return Err("title/base_ref apply to one PR, not to open:true".into());
    }
    Ok(())
}

async fn run_sync(
    state: &SharedState,
    body: SyncBody,
    job: Option<JobHandle>,
) -> Result<Value, ApiError> {
    match body.pr_number {
        Some(pr) => {
            sync_one(
                state,
                SyncRequest {
                    repo: body.repo,
                    pr,
                    title: body.title,
                    base: body.base_ref,
                    dry_run: body.dry_run,
                    gh_token: body.gh_token,
                },
                job,
            )
            .await
        }
        None => {
            sync_open(
                state,
                &body.repo,
                body.merged_since,
                body.dry_run,
                body.gh_token,
                job,
            )
            .await
        }
    }
}

/// `POST /api/reviews/sync[?async=1]` — LOOPBACK-ONLY (it creates
/// reviews, fetches and captures, like `POST /api/reviews/pr`). `?async=1`
/// runs it as a daemon job (`GET /api/reviews/jobs/{id}`, kind `sync`).
pub async fn sync_route(
    State(state): State<SharedState>,
    axum::extract::ConnectInfo(peer): axum::extract::ConnectInfo<std::net::SocketAddr>,
    headers: axum::http::HeaderMap,
    Query(params): Query<StartPrParams>,
    Json(mut body): Json<SyncBody>,
) -> Result<Response, ApiError> {
    let is_loopback = kb_server::middleware::is_loopback_origin(
        Some(peer.ip()),
        &headers,
        &state.auth.trusted_proxies,
    );
    body.gh_token =
        match crate::github::admit_cli_github_token(is_loopback, body.gh_token.as_deref()) {
            Ok(t) => t,
            Err(msg) => return Err(ApiError::bad_request(msg)),
        };
    validate_sync_body(&body).map_err(ApiError::bad_request)?;
    find_repo(&state, &body.repo)?;
    if params.wants_async() {
        let repo = body.repo.clone();
        let key = body.pr_number.unwrap_or(0);
        return crate::review_jobs::start_job(state, "sync", repo, key, move |st, h| async move {
            run_sync(&st, body, Some(h))
                .await
                .map(|v| (StatusCode::OK, v))
        })
        .await;
    }
    let v = run_sync(&state, body, None).await?;
    Ok(([(header::CACHE_CONTROL, "no-store")], Json(v)).into_response())
}

// --- GET /api/reviews/{id}/status -------------------------------------------------------------

#[derive(Debug, Default, Deserialize)]
pub struct ReviewStatusParams {
    /// `1` = fetch base + PR head into the review store first
    /// (loopback-only).
    #[serde(default)]
    pub fetch: Option<String>,
}

impl ReviewStatusParams {
    pub fn wants_fetch(&self) -> bool {
        matches!(
            self.fetch.as_deref(),
            Some("1") | Some("true") | Some("yes")
        )
    }
}

/// `head_moved` is the forge head vs the LATEST PATCHSET TIP. Pure.
pub fn head_moved(remote_head: Option<&str>, latest_tip: Option<&str>) -> Option<bool> {
    match (remote_head, latest_tip) {
        (Some(r), Some(t)) => Some(r != t),
        _ => None,
    }
}

/// `?fetch=1`: base + PR head into the READY review store. `Ok(None)` =
/// no ready store (nothing fetched, nothing written anywhere).
async fn fetch_into_store(
    state: &SharedState,
    review: &ReviewRow,
    pr: u32,
) -> Result<Option<crate::review_base::capture::FetchReport>, ApiError> {
    let Some(handle) = reviews::admit_store(state, &review.repo).await? else {
        return Ok(None);
    };
    let member = reviews::store_member(state, &review.repo)?;
    let rid = review.id;
    let report = reviews::with_store_ctx(state, handle, member, move |ctx| {
        let branches: Vec<String> = ctx
            .store
            .get_review_base(rid)
            .ok()
            .flatten()
            .filter(|b| b.base_mode.as_deref() == Some("track"))
            .and_then(|b| b.base_branch)
            .into_iter()
            .collect();
        let access = ctx.access();
        ctx.fetch_forge(access.as_ref().map_err(String::as_str), &branches, Some(pr))
    })
    .await?;
    Ok(Some(report))
}

/// `GET /api/reviews/{id}/status[?fetch=1]` (`kbc-review-status/1`). See
/// the module doc. Bearer; `?fetch=1` refuses a non-loopback caller (403).
pub async fn review_status_route(
    State(state): State<SharedState>,
    axum::extract::ConnectInfo(peer): axum::extract::ConnectInfo<std::net::SocketAddr>,
    headers: axum::http::HeaderMap,
    AxumPath(id): AxumPath<i64>,
    Query(params): Query<ReviewStatusParams>,
) -> Result<Response, ApiError> {
    let fetch = params.wants_fetch();
    if fetch
        && !kb_server::middleware::is_loopback_origin(
            Some(peer.ip()),
            &headers,
            &state.auth.trusted_proxies,
        )
    {
        return Err(ApiError::new(
            StatusCode::FORBIDDEN,
            "?fetch=1 writes the review store and is loopback-only",
        )
        .with_problem_type(URN_FETCH_LOOPBACK_ONLY));
    }
    let (review, repo, _) = reviews::require_review(&state, id).await?;
    let repo = repo.clone();
    let rid = review.id;
    let (latest, binding, findings) = state
        .store
        .run_blocking(move |store| -> Result<_, ApiError> {
            Ok((
                store.latest_patchset(rid)?,
                store.get_review_pr_binding(rid)?.unwrap_or_default(),
                store.list_review_findings(rid, None, false)?,
            ))
        })
        .await?;
    let pr: Option<u32> = binding
        .pr_number
        .and_then(|n| u32::try_from(n).ok())
        .or_else(|| pr_of_head(&review.head_ref));

    let mut warnings: Vec<BaseWarningOut> = Vec::new();
    let mut remote_head: Option<String> = None;
    let mut remote_source: Option<&'static str> = None;
    let mut fetched = false;
    let mut forge: Option<ForgeOut> = None;
    if let Some(n) = pr {
        if fetch {
            match fetch_into_store(&state, &review, n).await {
                Ok(Some(report)) => {
                    fetched = report.fetched();
                    match report.pr_head {
                        Some(sha) => {
                            remote_head = Some(sha);
                            remote_source = Some("store-fetch");
                        }
                        None => warnings.push(warning(
                            sync_warn::FETCH_FAILED,
                            format!(
                                "PR #{n}'s head could not be fetched into the review store ({})",
                                report
                                    .pr_error
                                    .as_deref()
                                    .or(report.code.as_deref())
                                    .unwrap_or(report.state.as_str())
                            ),
                        )),
                    }
                }
                Ok(None) => warnings.push(warning(
                    sync_warn::FETCH_UNAVAILABLE,
                    "no ready review store for this repo; nothing was fetched",
                )),
                Err(e) => warnings.push(warning(sync_warn::FETCH_FAILED, e.message().to_string())),
            }
        }
        let (f, cred_warnings) = forge_pr(&state, &repo, n, None).await;
        warnings.extend(cred_warnings);
        if f.available {
            if remote_head.is_none() {
                remote_head = f.head_sha.clone();
                remote_source = f.head_sha.as_ref().map(|_| "forge-api");
            }
        } else {
            warnings.push(forge_warning(n, &f));
        }
        forge = Some(f);
    }

    let latest_tip = latest.as_ref().map(|p| p.tip_sha.clone());
    let moved = head_moved(remote_head.as_deref(), latest_tip.as_deref());

    let root = repo.path.clone();
    let rv = review.clone();
    let mb = latest.as_ref().map(|p| p.base_sha.clone());
    let (base, base_warnings) = state
        .store
        .run_blocking(move |store| reviews::review_base_block(store, &rv, &root, mb.as_deref()))
        .await;
    warnings.extend(base_warnings);

    let files = count_files(&state, &repo, latest.as_ref()).await;
    let forge_files = forge.as_ref().and_then(|f| f.changed_files);
    let equal = match (files, forge_files) {
        (Some(a), Some(b)) => Some(a as u64 == b),
        _ => None,
    };
    let latest_ps = latest.as_ref().map(|p| p.ps_number);
    let verdict_stale = review.verdict.is_some() && review.verdict_ps != latest_ps;
    let open_findings = findings
        .iter()
        .filter(|f| f.severity != "ok" && f.disposition.is_none())
        .count();

    let body = json!({
        "schema": STATUS_SCHEMA,
        "review_id": review.id,
        "repo": review.repo,
        "state": review.state,
        "pr_number": pr,
        "head_moved": moved,
        "remote_head": remote_head,
        "remote_head_source": remote_source,
        "fetched": fetched,
        "latest_ps": latest_ps,
        "latest_tip": latest_tip,
        "latest_merge_base": latest.as_ref().map(|p| p.base_sha.clone()),
        "pr_head_sha": binding.pr_head_sha,
        "base": base,
        "drift": {
            "files_count": files,
            "forge_changed_files": forge_files,
            "equal": equal,
        },
        "verdict": { "state": review.verdict, "ps": review.verdict_ps },
        "verdict_stale": verdict_stale,
        "findings_total": findings.len(),
        "open_findings": open_findings,
        "forge": forge,
        "warnings": warnings_json(&warnings),
    });
    Ok(([(header::CACHE_CONTROL, "no-store")], Json(body)).into_response())
}

// --- the surface declaration (crate invariant 15) ------------------------------

fn status_params_accept_without(_omit: &str) -> bool {
    serde_json::from_value::<ReviewStatusParams>(Value::Object(serde_json::Map::new())).is_ok()
}

pub const REVIEW_STATUS_ROUTE: RouteContract = RouteContract {
    path: "/api/reviews/{id}/status",
    handler: "review_sync::review_status_route",
    required_params: &[],
    params_accept_without: status_params_accept_without,
};

/// The read RS-U10b adds, walked from BOTH sides (router registration in
/// `entities`' test, a CLI request builder in kb-code-cli's). `POST
/// /api/reviews/sync` is absent for the reason `review_jobs::
/// V76_R1A_ROUTES` records for `POST /api/reviews/pr`: its contract is its
/// JSON body.
pub const RS_U10B_ROUTES: &[RouteContract] = &[REVIEW_STATUS_ROUTE];
