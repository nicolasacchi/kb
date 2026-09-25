//! RS-U6 — fetch + capture INSIDE the review store (README §3, §5.3, §6;
//! design-internal-store §4).
//!
//! * [`capture_at`] — the ONE capture primitive, over any classified root
//!   (the store once it is ready; the member clone as the pre-store
//!   fallback): `merge-base(T, head)` is computed FIRST, then the
//!   `(tip, merge-base)` pair decides whether anything is minted
//!   ([`super::decide_kind`], D13), then the `ps<n>` (+ `ps<n>-base` in the
//!   store) refs and the DB row are written.
//! * [`StoreCtx`] — one member of a READY store: the network/local fetch
//!   of base branches + the PR head (under the per-(store, remote) fetch
//!   lock), the member's `work-<id>` import (local), the policy → tip
//!   resolution, and capture under the store's `ops` lock.
//!   [`StoreCtx::prepare_new`] runs the resolution chain for a NEW review;
//!   [`StoreCtx::recapture`] re-captures an existing one (snapshot, reuse,
//!   auto-capture), following a PR retarget when `set_by=auto` (D15).
//!
//! # Fetch triggers (README §5.3)
//!
//! Explicit actions (create, start-pr, reuse, snapshot, `pr fetch`) fetch
//! `base` over the network with the store's resolved credential;
//! auto-capture (`head_moved`) NEVER does — it imports the member's
//! `work-<id>` heads (a local fetch) and resolves the base from what the
//! store already holds. A store whose project has no network forge but
//! whose member's `origin` (or single remote) is a LOCAL repository path
//! fetches `base` from that path with the `file`-only transport — a
//! local-path "forge" (a bare repo on a NAS, test fixtures).
//!
//! # Locks
//!
//! Synchronous throughout: the store's tokio mutexes are taken with
//! `blocking_lock()`, so call these from `spawn_blocking` (or a plain
//! `#[test]`), never from an async task. The fetch lock is never held
//! across the capture; the `ops` lock covers head/base resolution, the
//! merge-base, the ref writes and the DB insert.

use std::path::{Path, PathBuf};

use super::{
    classify_base, decide_kind, effective_base, pick_default_branch, resolve_non_pr_base,
    resolve_pr_base, valid_branch_name, warn, warning, BaseError, BaseMode, BasePolicy, BaseProbe,
    BaseSource, BaseStatus, BaseWarningOut, Classified, EffectiveBase, NonPrChain, PatchsetKind,
    PrChain, SetBy, URN_BASE_UNAVAILABLE, URN_CAPTURE_FAILED, URN_HEAD_UNAVAILABLE,
    URN_NO_MERGE_BASE, URN_PR_FETCH_FAILED, URN_PR_REFS_UNSUPPORTED,
};
use crate::git::roots::{GitRoot, StoreRoot, WorkTreeRoot};
use crate::git::Revspec;
use crate::review_store::cred::FetchCredential;
use crate::review_store::git::{
    FetchAuth, GitArgs, GitCall, StoreGit, BASE_FETCH_TIMEOUT, LS_REMOTE_TIMEOUT,
    WORK_FETCH_TIMEOUT,
};
use crate::review_store::key::store_key_for_url;
use crate::review_store::registry::{ReviewStores, StoreHandle};
use crate::review_store::seed::{self, SeedMember};
use crate::review_store::url::{FetchRefspec, RefName, RefSource, RemoteName, RemoteUrl};
use crate::reviews::{self, KbcRef, ReviewGitError};
use crate::store::{ReviewPatchsetRow, ReviewRow, ReviewStoreRow, Store};
use kb_core::events::EventBus;

fn now() -> i64 {
    chrono::Utc::now().timestamp()
}

fn db_err(e: impl std::fmt::Display) -> ReviewGitError {
    ReviewGitError::GitFailed {
        status: -1,
        stderr: e.to_string(),
    }
}

// --- the capture primitive -----------------------------------------------------

/// What a capture did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CaptureOutcome {
    /// The minted patchset, or the latest one when nothing was minted.
    pub ps: ReviewPatchsetRow,
    /// `false` = the `(tip, merge-base)` pair was unchanged (D13).
    pub minted: bool,
    pub kind: Option<String>,
    pub base_tip_sha: Option<String>,
}

/// Capture options.
#[derive(Debug, Clone, Copy, Default)]
pub struct CaptureOpts {
    /// Mint even when the pair is unchanged (`snapshot --force`, and every
    /// review creation).
    pub force: bool,
    /// Why, when the caller knows better than the pair diff
    /// (`base-corrected`, `retarget`).
    pub kind_hint: Option<PatchsetKind>,
}

/// Capture `tip` against base tip `base_tip` for `review` in `root`
/// (README §3). `merge-base(base_tip, tip)` is computed BEFORE the skip
/// check; a patchset is minted only when the `(tip, merge-base)` pair
/// differs from the latest one (or `opts.force`). `record_base_tip` =
/// write `base_tip_sha` + its `ps<n>-base` keep-alive ref (the store; a
/// member clone never gets one).
#[allow(clippy::too_many_arguments)]
pub fn capture_at(
    store: &Store,
    bus: &EventBus,
    root: &dyn GitRoot,
    review: &ReviewRow,
    tip: &str,
    base_tip: &str,
    record_base_tip: bool,
    opts: &CaptureOpts,
    max_patchsets: u32,
) -> Result<CaptureOutcome, ReviewGitError> {
    let merge_base = reviews::merge_base_sha(root, base_tip, tip)?;
    let latest = store.latest_patchset(review.id).map_err(db_err)?;
    let kind = decide_kind(
        latest
            .as_ref()
            .map(|l| (l.tip_sha.as_str(), l.base_sha.as_str())),
        tip,
        &merge_base,
        opts.kind_hint,
        opts.force,
    );
    let Some(kind) = kind else {
        let latest = latest.ok_or_else(|| db_err("skip without a latest patchset"))?;
        let fields = store
            .get_patchset_base(review.id, latest.ps_number)
            .map_err(db_err)?
            .unwrap_or_default();
        return Ok(CaptureOutcome {
            ps: latest,
            minted: false,
            kind: fields.kind,
            base_tip_sha: fields.base_tip_sha,
        });
    };

    // Reserve the number BEFORE GC so numbers stay monotonic.
    let ps_number = store.next_ps_number(review.id).map_err(db_err)?;
    while store.patchset_count(review.id).unwrap_or(0) as u32 >= max_patchsets.max(1) {
        let Ok(Some(old)) = store.oldest_patchset(review.id) else {
            break;
        };
        let _ = reviews::delete_patchset_ref(root, review.id, old.ps_number);
        let _ = reviews::delete_kbc_ref(
            root,
            KbcRef::PatchsetBase {
                review_id: review.id,
                ps_number: old.ps_number,
            },
        );
        if !store
            .delete_patchset(review.id, old.ps_number)
            .unwrap_or(false)
        {
            break;
        }
    }

    reviews::update_patchset_ref(root, review.id, ps_number, tip)?;
    let base_tip_sha = if record_base_tip {
        reviews::update_patchset_base_ref(root, review.id, ps_number, base_tip)?;
        Some(base_tip.to_string())
    } else {
        None
    };
    store
        .insert_patchset_with_base(
            review.id,
            ps_number,
            tip,
            &merge_base,
            base_tip_sha.as_deref(),
            Some(kind.as_str()),
            now(),
        )
        .map_err(db_err)?;
    reviews::emit_review_changed(bus, review.id, &review.repo, "patchset", false);
    let ps = store
        .get_patchset(review.id, ps_number)
        .map_err(db_err)?
        .ok_or_else(|| db_err("patchset row missing after insert"))?;
    Ok(CaptureOutcome {
        ps,
        minted: true,
        kind: Some(kind.as_str().to_string()),
        base_tip_sha,
    })
}

fn capture_error(e: ReviewGitError) -> BaseError {
    match e {
        ReviewGitError::NoMergeBase(a, b) => BaseError::new(
            400,
            URN_NO_MERGE_BASE,
            format!("the base {a} and the head {b} share no history"),
        ),
        other => BaseError::new(500, URN_CAPTURE_FAILED, other.to_string()),
    }
}

// --- member + forge -----------------------------------------------------------

/// The member clone a review belongs to.
#[derive(Debug, Clone)]
pub struct Member {
    pub id: i64,
    pub name: String,
    pub root: PathBuf,
}

/// Where the store's `base` remote fetches from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Forge {
    /// The store's canonical network URL (credentialed fetch).
    Network(RemoteUrl),
    /// A member remote that is a local repository path (`file` only).
    Local(PathBuf),
    /// Nothing to fetch a base from.
    None,
}

/// A member remote URL as a local repository path, if it is one.
fn local_repo_path(url: &str) -> Option<PathBuf> {
    let p = url.strip_prefix("file://").unwrap_or(url);
    let path = Path::new(p);
    if !path.is_absolute() {
        return None;
    }
    seed::common_dir_of(path).ok()
}

/// The member's local-path "forge": `origin`, else its only remote, when
/// that remote is a local repository path.
pub fn local_forge_path(remotes: &[(String, String)]) -> Option<(String, PathBuf)> {
    let pick = remotes
        .iter()
        .find(|(n, _)| n == "origin")
        .or_else(|| (remotes.len() == 1).then(|| &remotes[0]))?;
    local_repo_path(&pick.1).map(|p| (pick.0.clone(), p))
}

/// The member remotes that map to `row`'s project: same `store_key` for a
/// forge store; the local-path forge's remote for a `local:` store; and —
/// with no store row at all — `origin`, the pre-store assumption.
pub fn mapped_remote_names(
    remotes: &[(String, String)],
    row: Option<&ReviewStoreRow>,
) -> Vec<String> {
    match row {
        Some(r) if !crate::review_store::key::is_local_key(&r.store_key) => remotes
            .iter()
            .filter(|(_, url)| store_key_for_url(url).as_deref() == Some(r.store_key.as_str()))
            .map(|(n, _)| n.clone())
            .collect(),
        Some(_) => local_forge_path(remotes)
            .map(|(n, _)| vec![n])
            .unwrap_or_default(),
        None => remotes
            .iter()
            .filter(|(n, _)| n == "origin")
            .map(|(n, _)| n.clone())
            .collect(),
    }
}

/// [`mapped_remote_names`] for a read path (get/list): the member's own
/// config plus its store row, if any. Blocking.
pub fn read_mapped_remotes(store: &Store, repo_name: &str, root: &Path) -> Vec<String> {
    let remotes = reviews::member_remotes(&WorkTreeRoot::user_clone(root));
    let row = store.store_for_repo_name(repo_name).ok().flatten();
    mapped_remote_names(&remotes, row.as_ref())
}

/// The branch name a head ref names, if it is one (`feature`,
/// `refs/heads/feature`).
pub fn branch_of_head(head_ref: &str) -> Option<String> {
    let b = head_ref.strip_prefix("refs/heads/").unwrap_or(head_ref);
    valid_branch_name(b).then(|| b.to_string())
}

/// The PR number a review's head ref pins (`refs/kbc/pr/<n>`).
pub fn pr_of_head(head_ref: &str) -> Option<u32> {
    match reviews::parse_kbc_ref(head_ref) {
        Some(KbcRef::Pr { number }) => Some(number),
        _ => None,
    }
}

/// The forge's PR head ref for `n` (README §5.3). `None` = the forge has
/// no PR refs (`forge = "none"`). An unknown forge keeps the GitHub shape
/// every pre-store fetch used.
pub fn pr_head_source(forge_kind: Option<&str>, n: u32) -> Option<String> {
    match forge_kind {
        Some("none") => None,
        Some("gitlab") => Some(format!("refs/merge-requests/{n}/head")),
        Some("bitbucket-server") => Some(format!("refs/pull-requests/{n}/from")),
        _ => Some(format!("refs/pull/{n}/head")),
    }
}

// --- fetch ---------------------------------------------------------------------

/// How the store may reach its forge for one explicit action.
pub struct Access {
    cred: Option<FetchCredential>,
    local: bool,
    via: String,
}

impl Access {
    fn auth(&self) -> Option<FetchAuth<'_>> {
        if self.local {
            Some(FetchAuth::LocalOnly)
        } else {
            self.cred.as_ref().and_then(FetchCredential::auth)
        }
    }
}

/// What a `base` fetch did.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct FetchReport {
    /// `fetched` | `failed` | `offline` | `skipped` | `cached` (no fetch
    /// was attempted: auto-capture, `--no-fetch`).
    pub state: String,
    /// Failure class / skip reason slug.
    pub code: Option<String>,
    /// Requested base branches the forge does not have.
    pub vanished: Vec<String>,
    /// The PR head sha, when the PR ref fetched.
    pub pr_head: Option<String>,
    pub pr_error: Option<String>,
    pub via: Option<String>,
}

impl FetchReport {
    fn skipped(code: &str) -> Self {
        Self {
            state: "skipped".into(),
            code: Some(code.into()),
            ..Self::default()
        }
    }
    pub fn cached() -> Self {
        Self {
            state: "cached".into(),
            ..Self::default()
        }
    }
    pub fn fetched(&self) -> bool {
        self.state == "fetched"
    }
    fn warnings(&self) -> Vec<BaseWarningOut> {
        let mut w = Vec::new();
        match self.state.as_str() {
            "offline" => w.push(warning(
                warn::BASE_OFFLINE,
                "the forge could not be reached; the base is the review store's last fetched copy",
            )),
            "failed" => w.push(warning(
                warn::BASE_REFRESH_FAILED,
                format!(
                    "the base fetch failed ({}); the base is the review store's last fetched copy",
                    self.code.as_deref().unwrap_or("failed")
                ),
            )),
            _ => {}
        }
        if let Some(e) = &self.pr_error {
            w.push(warning(
                warn::PR_REFRESH_FAILED,
                format!("the PR head could not be refreshed ({e})"),
            ));
        }
        w
    }
}

/// One member of a ready store — see the module doc.
pub struct StoreCtx<'a> {
    pub rs: &'a ReviewStores,
    pub store: &'a Store,
    pub bus: &'a EventBus,
    pub handle: &'a StoreHandle,
    pub member: Member,
    pub max_patchsets: u32,
}

/// A new review, prepared (resolved + fetched) before its row exists, so a
/// failure never leaves a half-created review behind.
#[derive(Debug, Clone)]
pub struct Prepared {
    pub policy: BasePolicy,
    pub warnings: Vec<BaseWarningOut>,
    pub head_sha: String,
    pub base_tip: String,
    /// The `reviews.base_ref` display value.
    pub base_ref: String,
    pub fetch: FetchReport,
    pub status: BaseStatus,
}

/// A new review's inputs.
#[derive(Debug, Clone, Default)]
pub struct NewReview {
    /// Non-PR: the caller's head ref.
    pub head_ref: String,
    pub pr: Option<u32>,
    /// The `--base` grammar input.
    pub base_input: Option<String>,
    /// Forge API `base.ref` (read by the async caller).
    pub forge_base_ref: Option<String>,
    /// Caller-supplied target (`caller_base_ref`).
    pub caller_base_ref: Option<String>,
    /// The PR's head branch (API `head.ref`), excluded from every rung.
    pub pr_head_branch: Option<String>,
}

/// An existing review's re-capture.
#[derive(Debug, Clone, Default)]
pub struct Recapture {
    /// Fetch `base` (+ the PR head) over the network first (explicit
    /// actions). `false` = auto-capture / `--no-fetch`.
    pub network: bool,
    pub force: bool,
    /// Forge API `base.ref` for a PR-bound review (D15 retarget-follow).
    pub forge_base_ref: Option<String>,
    pub kind_hint: Option<PatchsetKind>,
    /// Replace the policy (an explicit re-base); persisted with the capture.
    pub policy_override: Option<BasePolicy>,
}

/// What [`StoreCtx::recapture`] did.
#[derive(Debug, Clone)]
pub struct Recaptured {
    pub outcome: CaptureOutcome,
    pub effective: EffectiveBase,
    pub warnings: Vec<BaseWarningOut>,
    pub status: BaseStatus,
    pub fetch: FetchReport,
    pub retargeted: bool,
}

/// Compose the stored `base_status` after a fetch.
pub fn status_after(
    fetch: &FetchReport,
    effective: &EffectiveBase,
    prev: &BaseStatus,
) -> BaseStatus {
    let (mode, source, branch) = match effective {
        EffectiveBase::Policy(p) => (Some(p.mode), Some(p.source), p.branch.clone()),
        EffectiveBase::Verbatim(_) => (None, None, None),
    };
    let vanished = branch.is_some_and(|b| fetch.vanished.contains(&b));
    let state = match (mode, fetch.state.as_str()) {
        (Some(BaseMode::Pin), _) => "pinned".to_string(),
        (None, _) => "legacy".to_string(),
        (_, _) if vanished => "base-vanished".to_string(),
        (_, "fetched") => "ok".to_string(),
        (_, "offline") => "offline".to_string(),
        (_, "failed") => "refresh-failed".to_string(),
        _ => prev.state.clone().unwrap_or_else(|| "cached".to_string()),
    };
    let fetched = fetch.fetched();
    BaseStatus {
        state: Some(state),
        source: source
            .map(|s| s.as_str().to_string())
            .or_else(|| prev.source.clone()),
        last_fetch: Some(fetch.state.clone()),
        fetched_at: if fetched {
            Some(now())
        } else {
            prev.fetched_at
        },
        via: fetch.via.clone().or_else(|| prev.via.clone()),
        code: fetch.code.clone(),
    }
}

struct StoreProbe<'a, 'b> {
    ctx: &'b StoreCtx<'a>,
    mapped: Vec<String>,
}

impl BaseProbe for StoreProbe<'_, '_> {
    fn mapped_remotes(&self) -> Vec<String> {
        self.mapped.clone()
    }
    fn local_branch_exists(&self, b: &str) -> bool {
        Revspec::parse(&format!("refs/heads/{b}"))
            .ok()
            .and_then(|r| reviews::resolve_commit_sha(&self.ctx.work_root(), &r).ok())
            .is_some()
    }
    fn remote_branch_exists(&self, b: &str) -> bool {
        self.ctx
            .store_sha(&format!("refs/remotes/base/{b}"))
            .is_some()
            || self.mapped.iter().any(|m| {
                Revspec::parse(&format!("refs/remotes/{m}/{b}"))
                    .ok()
                    .and_then(|r| reviews::resolve_commit_sha(&self.ctx.work_root(), &r).ok())
                    .is_some()
            })
    }
    fn resolve_rev(&self, rev: &Revspec) -> Option<String> {
        reviews::resolve_commit_sha(&self.ctx.work_root(), rev)
            .ok()
            .or_else(|| reviews::resolve_commit_sha(&self.ctx.root(), rev).ok())
    }
}

impl<'a> StoreCtx<'a> {
    pub fn git(&self) -> Result<&'a StoreGit, BaseError> {
        self.rs.git().ok_or_else(|| {
            BaseError::new(
                503,
                "urn:kb:errors:store-disabled",
                "the review store's git spawner is unavailable",
            )
        })
    }

    pub fn root(&self) -> StoreRoot {
        StoreRoot::from_handle(self.handle)
    }

    fn work_root(&self) -> WorkTreeRoot {
        WorkTreeRoot::user_clone(&self.member.root)
    }

    fn member_remotes(&self) -> Vec<(String, String)> {
        reviews::member_remotes(&self.work_root())
    }

    /// Where `base` fetches from.
    pub fn forge(&self) -> Forge {
        if let Some(u) = self
            .handle
            .base_url
            .as_deref()
            .and_then(|u| RemoteUrl::parse_remote(u).ok())
        {
            return Forge::Network(u);
        }
        match local_forge_path(&self.member_remotes()) {
            Some((_, p)) => Forge::Local(p),
            None => Forge::None,
        }
    }

    pub fn mapped_remotes(&self) -> Vec<String> {
        let row = self.store.get_review_store(self.handle.id).ok().flatten();
        mapped_remote_names(&self.member_remotes(), row.as_ref())
    }

    /// A full commit sha a store ref (or sha) resolves to.
    pub fn store_sha(&self, rev: &str) -> Option<String> {
        let spec = Revspec::parse(rev).ok()?;
        reviews::resolve_commit_sha(&self.root(), &spec).ok()
    }

    fn has_commit(&self, sha: &str) -> bool {
        self.store_sha(sha).as_deref() == Some(sha)
    }

    /// Resolve how `base` is reached for one explicit action (runs the
    /// credential ladder for a network forge; configures the `base` remote).
    pub fn access(&self) -> Result<Access, String> {
        let git = self.git().map_err(|e| e.code().to_string())?;
        let dir = &self.handle.git_dir;
        match self.forge() {
            Forge::Network(url) => {
                let row = self
                    .store
                    .get_review_store(self.handle.id)
                    .ok()
                    .flatten()
                    .ok_or_else(|| "store-missing".to_string())?;
                let res = self
                    .rs
                    .resolve_credential(self.store, &row, &self.member.name)
                    .map_err(|e| e.class().slug().to_string())?;
                git.configure_remote(dir, &RemoteName::base(), &url)
                    .map_err(|e| e.slug().to_string())?;
                let kind = res.credential.kind().slug();
                let via = match res.credential.account() {
                    Some(a) => format!("{kind} ({a})"),
                    None => kind.to_string(),
                };
                Ok(Access {
                    cred: Some(res.credential),
                    local: false,
                    via,
                })
            }
            Forge::Local(p) => {
                git.allow_local_source(&p)
                    .map_err(|_| "local-source-refused".to_string())?;
                let url = RemoteUrl::local_seed(&p).map_err(|_| "url-rejected".to_string())?;
                git.configure_remote(dir, &RemoteName::base(), &url)
                    .map_err(|e| e.slug().to_string())?;
                Ok(Access {
                    cred: None,
                    local: true,
                    via: "local".into(),
                })
            }
            Forge::None => Err("no-base-remote".into()),
        }
    }

    /// Fetch base `branches` (+ PR `pr`'s head) from `base` into the store,
    /// under the `base` fetch lock (README §5.3).
    pub fn fetch_forge(
        &self,
        access: Result<&Access, &str>,
        branches: &[String],
        pr: Option<u32>,
    ) -> FetchReport {
        let access = match access {
            Ok(a) => a,
            Err(code) => return FetchReport::skipped(code),
        };
        let Some(auth) = access.auth() else {
            return FetchReport {
                via: Some(access.via.clone()),
                ..FetchReport::skipped("no-credentials")
            };
        };
        let Ok(git) = self.git() else {
            return FetchReport::skipped("store-disabled");
        };
        // (refspec, Some(branch) | None for the PR head)
        let mut specs: Vec<(FetchRefspec, Option<String>)> = Vec::new();
        for b in branches {
            if let (Ok(src), Ok(dst)) = (
                RefName::branch(b),
                RefName::parse(&format!("refs/remotes/base/{b}")),
            ) {
                specs.push((
                    FetchRefspec::new(true, RefSource::Ref(src), dst),
                    Some(b.clone()),
                ));
            }
        }
        let mut pr_error = None;
        if let Some(n) = pr {
            match pr_head_source(self.handle.forge_kind.as_deref(), n)
                .and_then(|s| RefName::parse(&s).ok())
                .zip(RefName::parse(&reviews::pr_ref(n)).ok())
            {
                Some((src, dst)) => {
                    specs.push((FetchRefspec::new(true, RefSource::Ref(src), dst), None))
                }
                None => pr_error = Some("pr-refs-unsupported".to_string()),
            }
        }
        if specs.is_empty() {
            return FetchReport {
                pr_error,
                via: Some(access.via.clone()),
                ..FetchReport::skipped("nothing-to-fetch")
            };
        }
        let base = RemoteName::base();
        let lock = self.rs.fetch_lock(self.handle.id, &base);
        let _guard = lock.blocking_lock();
        let dir = &self.handle.git_dir;
        let all: Vec<FetchRefspec> = specs.iter().map(|(s, _)| s.clone()).collect();
        let mut report = FetchReport {
            via: Some(access.via.clone()),
            pr_error,
            ..FetchReport::default()
        };
        match git.fetch(dir, &base, &all, auth, BASE_FETCH_TIMEOUT) {
            Ok(_) => report.state = "fetched".into(),
            Err(e) if e.class == crate::review_store::FailureClass::Vanished => {
                // One by one, so a missing ref never blocks the rest.
                report.state = "fetched".into();
                for (spec, label) in &specs {
                    match git.fetch(
                        dir,
                        &base,
                        std::slice::from_ref(spec),
                        auth,
                        BASE_FETCH_TIMEOUT,
                    ) {
                        Ok(_) => {}
                        Err(e) if e.class == crate::review_store::FailureClass::Vanished => {
                            match label {
                                Some(b) => report.vanished.push(b.clone()),
                                None => report.pr_error = Some("pr-not-found".into()),
                            }
                        }
                        Err(e) => {
                            report.state = if e.class.is_transient() {
                                "offline".into()
                            } else {
                                "failed".into()
                            };
                            report.code = Some(e.slug().into());
                        }
                    }
                }
            }
            Err(e) => {
                report.state = if e.class.is_transient() {
                    "offline".into()
                } else {
                    "failed".into()
                };
                report.code = Some(e.slug().into());
                if pr.is_some() {
                    report.pr_error = Some(e.slug().into());
                }
            }
        }
        if let Some(n) = pr {
            if report.pr_error.is_none() {
                report.pr_head = self.store_sha(&reviews::pr_ref(n));
            }
        }
        report
    }

    /// Import the member's heads (+ review refs) into `work-<id>` — a LOCAL
    /// fetch, the only fetch auto-capture ever does (README §5.3).
    pub fn import_work(&self) -> Result<(), BaseError> {
        let git = self.git()?;
        let common = seed::common_dir_of(&self.member.root).map_err(|e| {
            BaseError::new(
                500,
                URN_HEAD_UNAVAILABLE,
                format!("the member clone is not a git repository: {}", e.kind()),
            )
        })?;
        let lock = self
            .rs
            .fetch_lock(self.handle.id, &RemoteName::work(self.member.id));
        let _guard = lock.blocking_lock();
        seed::import_member(
            git,
            &self.handle.git_dir,
            &SeedMember {
                repo_id: self.member.id,
                common_dir: common,
            },
            WORK_FETCH_TIMEOUT,
        )
        .map(|_| ())
        .map_err(|e| {
            BaseError::new(
                500,
                URN_HEAD_UNAVAILABLE,
                format!(
                    "importing the member's branches into the review store failed ({})",
                    e.slug()
                ),
            )
        })
    }

    /// `git ls-remote --symref base HEAD` → the forge's default branch.
    fn probe_symref(&self, access: &Access) -> Option<String> {
        let git = self.git().ok()?;
        let auth = access.auth()?;
        let args = GitArgs::new("ls-remote")
            .flag("--symref")
            .end_of_options()
            .remote(&RemoteName::base())
            .flag("HEAD");
        let out = git
            .run(
                GitCall::new("ls-remote", args)
                    .git_dir(&self.handle.git_dir)
                    .auth(auth)
                    .timeout(LS_REMOTE_TIMEOUT),
            )
            .ok()?;
        out.stdout_str().lines().find_map(|l| {
            let (target, name) = l.strip_prefix("ref: ")?.split_once('\t')?;
            if name.trim() != "HEAD" {
                return None;
            }
            target
                .trim()
                .strip_prefix("refs/heads/")
                .map(str::to_string)
        })
    }

    fn cached_default_branch(&self) -> Option<String> {
        let row = self.store.get_review_store(self.handle.id).ok().flatten()?;
        let v: serde_json::Value = serde_json::from_str(row.state_json.as_deref()?).ok()?;
        v.get("default_branch")?.as_str().map(str::to_string)
    }

    fn cache_default_branch(&self, b: &str) {
        let Ok(Some(row)) = self.store.get_review_store(self.handle.id) else {
            return;
        };
        let mut v: serde_json::Value = row
            .state_json
            .as_deref()
            .and_then(|s| serde_json::from_str(s).ok())
            .filter(serde_json::Value::is_object)
            .unwrap_or_else(|| serde_json::json!({}));
        v["default_branch"] = b.into();
        let _ = self
            .store
            .set_review_store_state(row.id, &row.state, Some(&v.to_string()));
    }

    fn store_branches(&self, prefix: &'static str) -> Vec<String> {
        let Ok(git) = self.git() else {
            return vec![];
        };
        seed::list_refs(git, &self.handle.git_dir, &[prefix])
            .map(|rows| {
                rows.into_iter()
                    .filter_map(|(_, r)| r.strip_prefix(prefix).map(str::to_string))
                    .collect()
            })
            .unwrap_or_default()
    }

    /// The project's default branch (README §6): config → the forge's
    /// `HEAD` (cached in the store) → exactly one of main/master/trunk/
    /// develop → refuse. With no forge: config → one of the four in the
    /// member → the member's current branch.
    pub fn default_branch(
        &self,
        access: Option<&Access>,
        head_branch: Option<&str>,
    ) -> Result<(String, Vec<BaseWarningOut>), BaseError> {
        let cfg = self.rs.settings().repo(&self.member.name).default_branch;
        if matches!(self.forge(), Forge::None) {
            let heads = self.store_branches_member();
            return match pick_default_branch(cfg.as_deref(), None, &heads, head_branch) {
                Ok(r) => Ok(r),
                Err(e) => crate::git::GitRepo::open(&self.member.root)
                    .ok()
                    .and_then(|g| g.head_info().ok())
                    .and_then(|h| h.branch)
                    .filter(|b| valid_branch_name(b) && Some(b.as_str()) != head_branch)
                    .map(|b| (b, vec![]))
                    .ok_or(e),
            };
        }
        let symref = if cfg.is_some() {
            None
        } else {
            self.cached_default_branch().or_else(|| {
                let b = access.and_then(|a| self.probe_symref(a));
                if let Some(b) = &b {
                    self.cache_default_branch(b);
                }
                b
            })
        };
        let candidates = self.store_branches("refs/remotes/base/");
        pick_default_branch(cfg.as_deref(), symref.as_deref(), &candidates, head_branch)
    }

    fn store_branches_member(&self) -> Vec<String> {
        let prefix = format!("refs/remotes/work-{}/", self.member.id);
        let Ok(git) = self.git() else {
            return vec![];
        };
        seed::list_refs(git, &self.handle.git_dir, &["refs/remotes/"])
            .map(|rows| {
                rows.into_iter()
                    .filter_map(|(_, r)| r.strip_prefix(prefix.as_str()).map(str::to_string))
                    .collect()
            })
            .unwrap_or_default()
    }

    /// The head branch's `@{upstream}` as `(branch, maps_to_project)`. The
    /// head's own mirror (`feature` ↔ `origin/feature`) is dropped by the
    /// chain's head-branch exclusion.
    fn upstream_of(&self, head_branch: Option<&str>, mapped: &[String]) -> Option<(String, bool)> {
        let head = head_branch?;
        let up = reviews::branch_upstream(&self.work_root(), head)?;
        if let Some(rest) = up.strip_prefix("refs/remotes/") {
            let names: Vec<String> = self.member_remotes().into_iter().map(|(n, _)| n).collect();
            let (remote, b) = super::split_remote_branch(rest, &names)?;
            return Some((b, mapped.contains(&remote)));
        }
        up.strip_prefix("refs/heads/")
            .map(|b| (b.to_string(), false))
    }

    /// The base tip `T` an effective base resolves to in the store.
    pub fn base_tip(&self, eff: &EffectiveBase) -> Result<String, BaseError> {
        let unavailable = |what: String| BaseError::new(409, URN_BASE_UNAVAILABLE, what);
        match eff {
            EffectiveBase::Policy(p) => match p.mode {
                BaseMode::Track => {
                    let b = p.branch.as_deref().unwrap_or("");
                    self.store_sha(&format!("refs/remotes/base/{b}")).ok_or_else(|| {
                        unavailable(format!(
                            "the base branch {b:?} is not in the review store (never fetched, or gone from the forge) — run `kb-code review snapshot` with the forge reachable, or pass --base"
                        ))
                    })
                }
                BaseMode::Local => {
                    let b = p.branch.as_deref().unwrap_or("");
                    let m = p.member.unwrap_or(self.member.id);
                    self.store_sha(&format!("refs/remotes/work-{m}/{b}"))
                        .ok_or_else(|| {
                            unavailable(format!("the local base branch {b:?} does not exist"))
                        })
                }
                BaseMode::Pin => {
                    let sha = p.pin.as_deref().unwrap_or("");
                    if self.has_commit(sha) {
                        Ok(sha.to_string())
                    } else {
                        Err(unavailable(format!(
                            "the pinned base {sha} is not in the review store"
                        )))
                    }
                }
            },
            EffectiveBase::Verbatim(r) => {
                let spec = reviews::parse_user_ref(r).map_err(|_| {
                    unavailable(format!("the legacy base {r:?} is not a valid ref"))
                })?;
                let sha = reviews::resolve_commit_sha(&self.work_root(), &spec)
                    .ok()
                    .or_else(|| reviews::resolve_commit_sha(&self.root(), &spec).ok())
                    .ok_or_else(|| {
                        unavailable(format!("the legacy base {r:?} does not resolve"))
                    })?;
                if self.has_commit(&sha) {
                    Ok(sha)
                } else {
                    Err(unavailable(format!(
                        "the legacy base {r:?} ({sha}) is not in the review store"
                    )))
                }
            }
        }
    }

    /// The head tip: the fetched PR ref for a PR review, else the member's
    /// ref resolved read-only in the clone and required to be in the store.
    pub fn head_tip(&self, head_ref: &str) -> Result<String, BaseError> {
        let unavailable = |m: String| BaseError::new(409, URN_HEAD_UNAVAILABLE, m);
        if let Some(n) = pr_of_head(head_ref) {
            return self.store_sha(&reviews::pr_ref(n)).ok_or_else(|| {
                unavailable(format!(
                    "PR #{n}'s head is not in the review store — fetch it (`kb-code review snapshot` / `pr fetch`)"
                ))
            });
        }
        let spec = reviews::parse_user_ref(head_ref)
            .map_err(|_| unavailable(format!("{head_ref:?} is not a valid ref")))?;
        let sha = reviews::resolve_commit_sha(&self.work_root(), &spec)
            .map_err(|_| unavailable(format!("could not resolve {head_ref:?}")))?;
        if self.has_commit(&sha) {
            Ok(sha)
        } else {
            Err(unavailable(format!(
                "{head_ref:?} ({sha}) is not reachable from any branch imported into the review store"
            )))
        }
    }

    /// Capture under the `ops` lock: head and base are (re-)resolved inside
    /// it, so nothing GC reads can move between resolution and the write.
    pub fn capture(
        &self,
        review: &ReviewRow,
        eff: &EffectiveBase,
        opts: &CaptureOpts,
    ) -> Result<CaptureOutcome, BaseError> {
        let lock = self.rs.ops_lock(self.handle.id);
        let _guard = lock.blocking_lock();
        let head = self.head_tip(&review.head_ref)?;
        let t = self.base_tip(eff)?;
        capture_at(
            self.store,
            self.bus,
            &self.root(),
            review,
            &head,
            &t,
            true,
            opts,
            self.max_patchsets,
        )
        .map_err(capture_error)
    }

    /// Resolve + fetch a NEW review (README §6 chain, §5.3 create trigger).
    pub fn prepare_new(&self, nr: &NewReview) -> Result<Prepared, BaseError> {
        self.import_work()?;
        let mapped = self.mapped_remotes();
        let probe = StoreProbe {
            ctx: self,
            mapped: mapped.clone(),
        };
        let explicit = classify_base(nr.base_input.as_deref(), nr.pr.is_some(), &probe)?;
        let forge = self.forge();
        let has_forge = !matches!(forge, Forge::None);
        if nr.pr.is_some() && !has_forge {
            return Err(BaseError::new(
                400,
                URN_PR_REFS_UNSUPPORTED,
                "this repo has no forge remote to fetch PR refs from — review the branch with `kb-code review start <remote>/<branch>`",
            ));
        }
        let access = if has_forge { Some(self.access()) } else { None };
        let access_ok = access.as_ref().and_then(|a| a.as_ref().ok());
        let (policy, mut warnings) = match nr.pr {
            Some(_) => {
                let head_branch = nr.pr_head_branch.clone();
                let chain = PrChain {
                    explicit: Some(explicit),
                    forge_api: nr.forge_base_ref.clone(),
                    caller: nr.caller_base_ref.clone(),
                    head_branch: head_branch.clone(),
                };
                resolve_pr_base(&chain, || {
                    self.default_branch(access_ok, head_branch.as_deref())
                })?
            }
            None => {
                let head_branch = branch_of_head(&nr.head_ref);
                let chain = NonPrChain {
                    explicit: Some(explicit),
                    stack_parent: None,
                    upstream: self.upstream_of(head_branch.as_deref(), &mapped),
                    head_branch: head_branch.clone(),
                    has_forge,
                };
                resolve_non_pr_base(&chain, || {
                    self.default_branch(access_ok, head_branch.as_deref())
                })?
            }
        };
        let branches: Vec<String> = match policy.mode {
            BaseMode::Track => policy.branch.iter().cloned().collect(),
            _ => vec![],
        };
        let fetch = match &access {
            None => FetchReport::skipped("no-base-remote"),
            Some(_) if branches.is_empty() && nr.pr.is_none() => FetchReport::cached(),
            Some(a) => self.fetch_forge(a.as_ref().map_err(String::as_str), &branches, nr.pr),
        };
        warnings.extend(fetch.warnings());
        if let Some(b) = branches.first() {
            if fetch.vanished.contains(b) {
                return Err(BaseError::unresolved(format!(
                    "the base branch {b:?} does not exist on the forge"
                )));
            }
        }
        let head_sha = match nr.pr {
            Some(n) => fetch.pr_head.clone().ok_or_else(|| {
                BaseError::new(
                    400,
                    URN_PR_FETCH_FAILED,
                    format!(
                        "PR fetch failed: PR #{n}'s head could not be fetched into the review store ({})",
                        fetch
                            .pr_error
                            .as_deref()
                            .or(fetch.code.as_deref())
                            .unwrap_or("failed")
                    ),
                )
            })?,
            None => self.head_tip(&nr.head_ref)?,
        };
        let eff = EffectiveBase::Policy(policy.clone());
        let base_tip = self.base_tip(&eff)?;
        reviews::merge_base_sha(&self.root(), &base_tip, &head_sha).map_err(capture_error)?;
        let status = status_after(&fetch, &eff, &BaseStatus::default());
        Ok(Prepared {
            base_ref: policy.display_base_ref(mapped.first().map(String::as_str)),
            policy,
            warnings,
            head_sha,
            base_tip,
            fetch,
            status,
        })
    }

    /// The `--base` grammar against this member + store (for `retrack`,
    /// RS-U7): `Ok(policy: None)` = `auto`. Blocking.
    pub fn classify(&self, input: Option<&str>, is_pr: bool) -> Result<Classified, BaseError> {
        let probe = StoreProbe {
            ctx: self,
            mapped: self.mapped_remotes(),
        };
        classify_base(input, is_pr, &probe)
    }

    /// Persist a policy + status on review `id`.
    pub fn persist_policy(&self, id: i64, policy: &BasePolicy, status: &BaseStatus) {
        let _ = self.store.set_review_base(
            id,
            policy.mode.as_str(),
            policy.branch.as_deref(),
            policy.member,
            policy.set_by.as_str(),
            Some(&status.to_json()),
        );
    }

    /// Re-capture an existing review in the store — snapshot, start-pr
    /// reuse, auto-capture (README §5.3 triggers; D13 dedup; D15
    /// retarget-follow for `set_by=auto`).
    pub fn recapture(&self, review: &ReviewRow, rc: &Recapture) -> Result<Recaptured, BaseError> {
        let base_row = self.store.get_review_base(review.id).ok().flatten();
        let binding = self
            .store
            .get_review_pr_binding(review.id)
            .ok()
            .flatten()
            .unwrap_or_default();
        let pr = pr_of_head(&review.head_ref);
        let meta_base: Option<String> = binding
            .pr_meta_json
            .as_deref()
            .and_then(|s| serde_json::from_str::<serde_json::Value>(s).ok())
            .and_then(|v| {
                v.get("base_ref")
                    .and_then(|b| b.as_str())
                    .map(str::to_string)
            });
        let mapped = self.mapped_remotes();
        let prev_status =
            BaseStatus::parse(base_row.as_ref().and_then(|b| b.base_status.as_deref()));
        let class = effective_base(
            &review.base_ref,
            base_row.as_ref().and_then(|b| b.base_mode.as_deref()),
            base_row.as_ref().and_then(|b| b.base_branch.as_deref()),
            base_row.as_ref().and_then(|b| b.base_member),
            base_row
                .as_ref()
                .map(|b| b.base_set_by.as_str())
                .unwrap_or("legacy"),
            prev_status.source.as_deref(),
            pr.is_some(),
            meta_base.as_deref(),
            &mapped,
        );
        let has_columns = base_row.as_ref().is_some_and(|b| b.base_mode.is_some());
        let mut warnings = class.warnings.clone();
        let mut kind_hint = rc.kind_hint;
        let mut persist = rc.policy_override.is_some();
        let mut retargeted = false;
        let mut effective = match &rc.policy_override {
            Some(p) => EffectiveBase::Policy(p.clone()),
            None => class.effective.clone(),
        };
        // D15 — follow a PR retarget when kb chose the base.
        if let (Some(api), Some(p), Some(_)) = (
            rc.forge_base_ref
                .as_deref()
                .filter(|b| valid_branch_name(b)),
            effective.policy().cloned(),
            pr,
        ) {
            if p.mode == BaseMode::Track && p.branch.as_deref() != Some(api) {
                if p.set_by == SetBy::Auto && rc.policy_override.is_none() {
                    warnings.push(warning(
                        warn::RETARGETED,
                        format!(
                            "the PR now targets {api:?} (was {:?}); the review follows it",
                            p.branch.as_deref().unwrap_or("")
                        ),
                    ));
                    effective = EffectiveBase::Policy(BasePolicy::track(
                        api,
                        SetBy::Auto,
                        BaseSource::ForgeApi,
                    ));
                    kind_hint = Some(PatchsetKind::Retarget);
                    persist = true;
                    retargeted = true;
                } else {
                    warnings.push(warning(
                        warn::PR_TARGET_DIFFERS,
                        format!(
                            "the PR targets {api:?} but this review compares with {:?} (set by a person; retrack to follow the PR)",
                            p.branch.as_deref().unwrap_or("")
                        ),
                    ));
                }
            }
        }
        let has_forge = !matches!(self.forge(), Forge::None);
        let fetch = if rc.network && has_forge {
            let branches: Vec<String> = effective
                .policy()
                .filter(|p| p.mode == BaseMode::Track)
                .and_then(|p| p.branch.clone())
                .into_iter()
                .collect();
            let access = self.access();
            self.fetch_forge(access.as_ref().map_err(String::as_str), &branches, pr)
        } else {
            FetchReport::cached()
        };
        warnings.extend(fetch.warnings());
        self.import_work()?;
        if let Some(sha) = &fetch.pr_head {
            let _ = self.store.set_review_pr_head_sha(review.id, sha);
        }
        if class.upgraded && fetch.fetched() && rc.policy_override.is_none() {
            persist = true;
        }
        let outcome = self.capture(
            review,
            &effective,
            &CaptureOpts {
                force: rc.force,
                kind_hint,
            },
        )?;
        let status = status_after(&fetch, &effective, &prev_status);
        if let EffectiveBase::Policy(p) = &effective {
            if persist || has_columns {
                self.persist_policy(review.id, p, &status);
            }
            if persist {
                let _ = self.store.set_review_base_ref(
                    review.id,
                    &p.display_base_ref(mapped.first().map(String::as_str)),
                );
            }
        }
        Ok(Recaptured {
            outcome,
            effective,
            warnings,
            status,
            fetch,
            retargeted,
        })
    }
}
