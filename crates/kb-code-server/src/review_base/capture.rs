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
use crate::review_store::seed;
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
    /// RS-U7 — `pub` so `review_retrack`'s dry-run path (which fetches but
    /// never captures, so it never sees these through [`Recaptured`]) can
    /// surface an offline/refresh-failed fetch honestly instead of silently
    /// classifying against stale data.
    pub fn warnings(&self) -> Vec<BaseWarningOut> {
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
    /// The `--base` grammar input — ONLY a caller-supplied `--base`; an
    /// auto-detected answer never rides here (it would be recorded as
    /// `set_by=user`, `source=explicit`).
    pub base_input: Option<String>,
    /// Forge API `base.ref` (read by the async caller).
    pub forge_base_ref: Option<String>,
    /// Caller-supplied target (`caller_base_ref`).
    pub caller_base_ref: Option<String>,
    /// The PR's head branch (API `head.ref`), excluded from every rung.
    pub pr_head_branch: Option<String>,
    /// Non-PR: the stack-parent rung's answer (`branch review`'s own stack
    /// ladder, `history::stacks`), recorded `local(parent)`, `auto`,
    /// `stack-parent`.
    pub stack_parent: Option<String>,
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
    /// Warnings the async caller collected while reading the forge API
    /// (e.g. `credential-account-mismatch`, D12) — carried onto the
    /// envelope and into `base_status.code`.
    pub api_warnings: Vec<BaseWarningOut>,
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

/// How long a cached forge default branch (`ls-remote --symref`) is
/// trusted before it is probed again.
pub const DEFAULT_BRANCH_TTL_SECS: i64 = 24 * 60 * 60;

/// The scratch hint branch (`refs/kbc/hint/<repo_id>/<name>`) a head or
/// base that is neither a local nor a remote-tracking branch of the member
/// (a sha, a tag, an expression) is imported into BY SHA — one per role,
/// overwritten each time; the patchset refs pin whatever a capture keeps.
const SCRATCH_HEAD: &str = "_head";
const SCRATCH_BASE: &str = "_base";

struct StoreProbe<'a, 'b> {
    ctx: &'b StoreCtx<'a>,
    mapped: Vec<String>,
    access: std::cell::OnceCell<Result<Access, String>>,
}

impl<'a, 'b> StoreProbe<'a, 'b> {
    fn new(ctx: &'b StoreCtx<'a>) -> Self {
        Self {
            ctx,
            mapped: ctx.mapped_remotes(),
            access: std::cell::OnceCell::new(),
        }
    }

    /// The forge access, resolved once and only when something needs it
    /// (the credential ladder can run `gh`).
    fn access(&self) -> Result<&Access, &str> {
        self.access
            .get_or_init(|| match self.ctx.forge() {
                Forge::None => Err("no-base-remote".to_string()),
                _ => self.ctx.access(),
            })
            .as_ref()
            .map_err(String::as_str)
    }
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
        if self
            .ctx
            .store_sha(&format!("refs/remotes/base/{b}"))
            .is_some()
            || self.mapped.iter().any(|m| {
                Revspec::parse(&format!("refs/remotes/{m}/{b}"))
                    .ok()
                    .and_then(|r| reviews::resolve_commit_sha(&self.ctx.work_root(), &r).ok())
                    .is_some()
            })
        {
            return true;
        }
        // Not fetched yet: ask the forge (fetch-then-track, never a 400 for
        // a branch that exists upstream but was never fetched here).
        match self.access() {
            Ok(a) => self.ctx.forge_has_branch(a, b),
            Err(_) => false,
        }
    }
    fn resolve_rev(&self, rev: &Revspec) -> Option<String> {
        reviews::resolve_commit_sha(&self.ctx.work_root(), rev)
            .ok()
            .or_else(|| reviews::resolve_commit_sha(&self.ctx.root(), rev).ok())
    }
}

/// The fixed per-invocation SOURCE-side override that lets a local fetch
/// ask a member clone for an object by id (same flag seeding's by-sha
/// recovery uses; nothing is written to the member's config).
const UPLOAD_PACK_ANY_SHA: &str =
    "--upload-pack=git -c uploadpack.allowAnySHA1InWant=true upload-pack";

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

    /// Where `base` fetches from. A local-path "forge" is admitted ONLY for
    /// a `local:` store — a networked store never fetches `base/*` from a
    /// path.
    pub fn forge(&self) -> Forge {
        if let Some(u) = self
            .handle
            .base_url
            .as_deref()
            .and_then(|u| RemoteUrl::parse_remote(u).ok())
        {
            return Forge::Network(u);
        }
        if !crate::review_store::key::is_local_key(&self.handle.store_key) {
            return Forge::None;
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

    /// Does the forge have branch `b`? (`ls-remote base refs/heads/<b>`.)
    fn forge_has_branch(&self, access: &Access, b: &str) -> bool {
        let (Ok(git), Some(auth), Ok(r)) = (self.git(), access.auth(), RefName::branch(b)) else {
            return false;
        };
        let args = GitArgs::new("ls-remote")
            .end_of_options()
            .remote(&RemoteName::base())
            .refname(&r);
        git.run(
            GitCall::new("ls-remote", args)
                .git_dir(&self.handle.git_dir)
                .auth(auth)
                .timeout(LS_REMOTE_TIMEOUT),
        )
        .is_ok_and(|o| !o.stdout_str().trim().is_empty())
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
        let mut pr_spec = false;
        if let Some(n) = pr {
            match pr_head_source(self.handle.forge_kind.as_deref(), n)
                .and_then(|s| RefName::parse(&s).ok())
                .zip(RefName::parse(&reviews::pr_ref(n)).ok())
            {
                Some((src, dst)) => {
                    specs.push((FetchRefspec::new(true, RefSource::Ref(src), dst), None));
                    pr_spec = true;
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
        // The PR head counts as refreshed ONLY when a fetch that carried
        // its refspec succeeded — never inferred from "no error recorded".
        let mut pr_fetched = false;
        let fail_state = |e: &crate::review_store::StoreGitError| {
            if e.class.is_transient() {
                "offline"
            } else {
                "failed"
            }
        };
        match git.fetch(dir, &base, &all, auth, BASE_FETCH_TIMEOUT) {
            Ok(_) => {
                report.state = "fetched".into();
                pr_fetched = pr_spec;
            }
            Err(e) if e.class == crate::review_store::FailureClass::Vanished => {
                // One by one, so a missing ref never blocks the rest.
                report.state = "fetched".into();
                for (spec, label) in &specs {
                    match (
                        git.fetch(
                            dir,
                            &base,
                            std::slice::from_ref(spec),
                            auth,
                            BASE_FETCH_TIMEOUT,
                        ),
                        label,
                    ) {
                        (Ok(_), None) => pr_fetched = true,
                        (Ok(_), Some(_)) => {}
                        (Err(e), Some(b))
                            if e.class == crate::review_store::FailureClass::Vanished =>
                        {
                            report.vanished.push(b.clone())
                        }
                        (Err(e), None)
                            if e.class == crate::review_store::FailureClass::Vanished =>
                        {
                            report.pr_error = Some("pr-not-found".into())
                        }
                        (Err(e), label) => {
                            report.state = fail_state(&e).into();
                            report.code = Some(e.slug().into());
                            if label.is_none() {
                                report.pr_error = Some(e.slug().into());
                            }
                        }
                    }
                }
            }
            Err(e) => {
                report.state = fail_state(&e).into();
                report.code = Some(e.slug().into());
                if pr_spec {
                    report.pr_error = Some(e.slug().into());
                }
            }
        }
        if let (Some(n), true) = (pr, pr_fetched) {
            report.pr_head = self.store_sha(&reviews::pr_ref(n));
        }
        report
    }

    /// One LOCAL fetch from the member clone (`work-<id>`, `file` only)
    /// under that member's fetch lock. `by_sha` = the source is an object
    /// id (the source-side `allowAnySHA1InWant` override).
    fn fetch_from_member(&self, spec: &FetchRefspec, by_sha: bool) -> Result<(), BaseError> {
        let git = self.git()?;
        let fail = |detail: String| {
            BaseError::new(
                409,
                URN_HEAD_UNAVAILABLE,
                format!("importing from the member clone into the review store failed ({detail})"),
            )
        };
        let common = seed::common_dir_of(&self.member.root).map_err(|e| fail(e.to_string()))?;
        git.allow_local_source(&common)
            .map_err(|e| fail(e.to_string()))?;
        let remote = RemoteName::work(self.member.id);
        let url = RemoteUrl::local_seed(&common).map_err(|e| fail(e.to_string()))?;
        let lock = self.rs.fetch_lock(self.handle.id, &remote);
        let _guard = lock.blocking_lock();
        git.configure_remote(&self.handle.git_dir, &remote, &url)
            .map_err(|e| fail(e.slug().to_string()))?;
        let mut args = GitArgs::new("fetch")
            .flag("--no-tags")
            .flag("--no-write-fetch-head")
            .flag("--no-auto-gc")
            .flag("--no-auto-maintenance")
            .flag("--quiet");
        if by_sha {
            args = args.flag(UPLOAD_PACK_ANY_SHA);
        }
        let args = args.end_of_options().remote(&remote).refspec(spec);
        git.run(
            GitCall::new("fetch", args)
                .git_dir(&self.handle.git_dir)
                .auth(FetchAuth::LocalOnly)
                .timeout(WORK_FETCH_TIMEOUT),
        )
        .map(|_| ())
        .map_err(|e| fail(e.slug().to_string()))
    }

    /// Import the member's LOCAL branch `b` into `refs/remotes/work-<id>/b`
    /// (a local fetch of exactly that branch — never `refs/kbc/*`, never
    /// every head). `Ok(None)` when the member has no such branch.
    pub fn import_branch(&self, b: &str) -> Result<Option<String>, BaseError> {
        let Ok(src) = RefName::branch(b) else {
            return Ok(None);
        };
        let Some(sha) = Revspec::parse(src.as_str())
            .ok()
            .and_then(|r| reviews::resolve_commit_sha(&self.work_root(), &r).ok())
        else {
            return Ok(None);
        };
        let dst = RefName::parse(&format!("refs/remotes/work-{}/{b}", self.member.id))
            .map_err(|_| BaseError::unresolved(format!("{b:?} is not a valid branch name")))?;
        self.fetch_from_member(&FetchRefspec::new(true, RefSource::Ref(src), dst), false)?;
        Ok(Some(sha))
    }

    /// Import the commit a member-side rev names (README §5.3: explicit
    /// actions import the head branch, auto-capture "the resolved tip
    /// only"): a local branch → `work-<id>/<b>`; a remote-tracking branch
    /// (`origin/feature`) → `refs/kbc/hint/<id>/origin/feature`; anything
    /// else (a sha, a tag, an expression) by object id into the scratch
    /// hint `scratch`. Never fetches `refs/kbc/*` from the clone. Returns
    /// the full sha, which is then in the store.
    fn import_rev(&self, rev: &str, scratch: &str) -> Result<String, BaseError> {
        let unavailable = |m: String| BaseError::new(409, URN_HEAD_UNAVAILABLE, m);
        let spec = reviews::parse_user_ref(rev)
            .map_err(|_| unavailable(format!("{rev:?} is not a valid ref")))?;
        let sha = reviews::resolve_commit_sha(&self.work_root(), &spec)
            .map_err(|_| unavailable(format!("could not resolve {rev:?} in the member clone")))?;
        if self.has_commit(&sha) {
            return Ok(sha);
        }
        let full = reviews::symbolic_full_name(&self.work_root(), &spec);
        match full.as_deref() {
            Some(r) if r.starts_with("refs/heads/") => {
                self.import_branch(&r["refs/heads/".len()..])?;
            }
            Some(r) if r.starts_with("refs/remotes/") => {
                let rest = &r["refs/remotes/".len()..];
                let (Ok(src), Some(dst)) = (
                    RefName::parse(r),
                    reviews::hint_ref(self.member.id, rest).and_then(|h| RefName::parse(&h).ok()),
                ) else {
                    return Err(unavailable(format!("{r:?} cannot be imported")));
                };
                self.fetch_from_member(&FetchRefspec::new(true, RefSource::Ref(src), dst), false)?;
            }
            _ => {
                let (Ok(src), Some(dst)) = (
                    RefSource::oid(&sha),
                    reviews::hint_ref(self.member.id, scratch)
                        .and_then(|h| RefName::parse(&h).ok()),
                ) else {
                    return Err(unavailable(format!("{sha} cannot be imported")));
                };
                self.fetch_from_member(&FetchRefspec::new(true, src, dst), true)?;
            }
        }
        if self.has_commit(&sha) {
            Ok(sha)
        } else {
            Err(unavailable(format!(
                "{rev:?} ({sha}) could not be imported into the review store"
            )))
        }
    }

    /// Import a review's HEAD (non-PR) into the store; see [`Self::import_rev`].
    pub fn import_head(&self, head_ref: &str) -> Result<String, BaseError> {
        self.import_rev(head_ref, SCRATCH_HEAD)
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

    /// The cached forge default branch, if it is younger than
    /// [`DEFAULT_BRANCH_TTL_SECS`].
    fn cached_default_branch(&self) -> Option<String> {
        let row = self.store.get_review_store(self.handle.id).ok().flatten()?;
        let v: serde_json::Value = serde_json::from_str(row.state_json.as_deref()?).ok()?;
        let at = v.get("default_branch_at")?.as_i64()?;
        if now() - at > DEFAULT_BRANCH_TTL_SECS {
            return None;
        }
        v.get("default_branch")?.as_str().map(str::to_string)
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
    /// `HEAD` symref (cached in the store for a day) → exactly one of
    /// main/master/trunk/develop, with `default-branch-guessed` → refuse.
    /// With no forge the candidates are the member's own branches and the
    /// ladder still REFUSES rather than taking whatever is checked out.
    pub fn default_branch(
        &self,
        access: Option<&Access>,
        head_branch: Option<&str>,
    ) -> Result<(String, Vec<BaseWarningOut>), BaseError> {
        let cfg = self.rs.settings().repo(&self.member.name).default_branch;
        if matches!(self.forge(), Forge::None) {
            let heads = reviews::member_branches(&self.work_root());
            return pick_default_branch(cfg.as_deref(), None, &heads, head_branch);
        }
        let symref = if cfg.is_some() {
            None
        } else {
            self.cached_default_branch().or_else(|| {
                let b = access.and_then(|a| self.probe_symref(a));
                if let Some(b) = &b {
                    let _ = self
                        .store
                        .set_review_store_default_branch(self.handle.id, b, now());
                }
                b
            })
        };
        let candidates = self.store_branches("refs/remotes/base/");
        pick_default_branch(cfg.as_deref(), symref.as_deref(), &candidates, head_branch)
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

    /// The base tip `T` an effective base resolves to in the store (no
    /// fetch, no import — [`Self::capture`] imports first).
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
    /// ref resolved read-only in the clone and required to be in the store
    /// (no import — [`Self::import_head`] does that).
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
                "{head_ref:?} ({sha}) is not in the review store"
            )))
        }
    }

    /// Import what the base needs from the member (a `local` branch, a pin
    /// or legacy rev not yet in the store). Fetch lock only; call BEFORE
    /// taking the ops lock (fetch → ops, never the reverse).
    fn import_base(&self, eff: &EffectiveBase) -> Result<(), BaseError> {
        match eff {
            EffectiveBase::Policy(p) => match p.mode {
                BaseMode::Local if p.member.unwrap_or(self.member.id) == self.member.id => {
                    if let Some(b) = &p.branch {
                        self.import_branch(b)?;
                    }
                }
                BaseMode::Pin => {
                    if let Some(sha) = p.pin.as_deref().filter(|s| !self.has_commit(s)) {
                        let _ = self.import_rev(sha, SCRATCH_BASE);
                    }
                }
                _ => {}
            },
            EffectiveBase::Verbatim(r) => {
                let _ = self.import_rev(r, SCRATCH_BASE);
            }
        }
        Ok(())
    }

    /// Capture under the `ops` lock: head and base are (re-)resolved inside
    /// it, so nothing GC reads can move between resolution and the write.
    /// The member-side imports (head, `local` base) run first, under the
    /// member's fetch lock only.
    pub fn capture(
        &self,
        review: &ReviewRow,
        eff: &EffectiveBase,
        opts: &CaptureOpts,
    ) -> Result<CaptureOutcome, BaseError> {
        self.capture_with(review, eff, opts, |_| {})
    }

    fn capture_with(
        &self,
        review: &ReviewRow,
        eff: &EffectiveBase,
        opts: &CaptureOpts,
        under_lock: impl FnOnce(&CaptureOutcome),
    ) -> Result<CaptureOutcome, BaseError> {
        let imported_head = match pr_of_head(&review.head_ref) {
            Some(_) => None,
            None => Some(self.import_head(&review.head_ref)?),
        };
        self.import_base(eff)?;
        let lock = self.rs.ops_lock(self.handle.id);
        let _guard = lock.blocking_lock();
        let head = match imported_head {
            Some(sha) if self.has_commit(&sha) => sha,
            Some(sha) => {
                return Err(BaseError::new(
                    409,
                    URN_HEAD_UNAVAILABLE,
                    format!("the head {sha} is no longer in the review store"),
                ))
            }
            None => self.head_tip(&review.head_ref)?,
        };
        let t = self.base_tip(eff)?;
        let out = capture_at(
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
        .map_err(capture_error)?;
        under_lock(&out);
        Ok(out)
    }

    /// Resolve + fetch a NEW review (README §6 chain, §5.3 create trigger).
    pub fn prepare_new(&self, nr: &NewReview) -> Result<Prepared, BaseError> {
        let probe = StoreProbe::new(self);
        let mapped = probe.mapped.clone();
        let explicit = classify_base(nr.base_input.as_deref(), nr.pr.is_some(), &probe)?;
        let has_forge = !matches!(self.forge(), Forge::None);
        if nr.pr.is_some() && !has_forge {
            return Err(BaseError::new(
                400,
                URN_PR_REFS_UNSUPPORTED,
                "this repo has no forge remote to fetch PR refs from — review the branch with `kb-code review start <remote>/<branch>`",
            ));
        }
        let access_ok = if has_forge { probe.access().ok() } else { None };
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
                    stack_parent: nr.stack_parent.clone(),
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
        let fetch = if !has_forge {
            FetchReport::skipped("no-base-remote")
        } else if branches.is_empty() && nr.pr.is_none() {
            FetchReport::cached()
        } else {
            self.fetch_forge(probe.access(), &branches, nr.pr)
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
            None => self.import_head(&nr.head_ref)?,
        };
        let eff = EffectiveBase::Policy(policy.clone());
        if policy.mode == BaseMode::Local {
            let b = policy.branch.as_deref().unwrap_or("");
            if self.import_branch(b)?.is_none() {
                return Err(BaseError::unresolved(format!(
                    "the local base branch {b:?} does not exist in the member clone"
                )));
            }
        } else {
            self.import_base(&eff)?;
        }
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

    /// RS-U7 — the resolution CHAIN only (README §6), no fetch, no
    /// capture: what [`prepare_new`] runs for a brand-new review's policy,
    /// factored out so `retrack` (README §10 step 4/§12) can re-run the
    /// SAME chain against an EXISTING review's current head, when the
    /// caller passed no explicit `--base` (bare retrack, or `--base auto`
    /// — [`classify`] already collapses both to `explicit.policy: None`,
    /// exactly the "run the chain" case). `classified` is [`classify`]'s
    /// own answer, so this never re-parses the grammar. Mirrors
    /// `prepare_new`'s `Some(_)`/`None` match byte-for-byte — kept as its
    /// own method rather than folding into `prepare_new` (which stays a
    /// RS-U6-shipped, tested entry point this unit does not touch).
    pub fn resolve_chain(
        &self,
        is_pr: bool,
        head_ref: &str,
        pr_head_branch: Option<&str>,
        forge_base_ref: Option<&str>,
        classified: Classified,
    ) -> Result<(BasePolicy, Vec<BaseWarningOut>), BaseError> {
        self.import_work()?;
        let mapped = self.mapped_remotes();
        let has_forge = !matches!(self.forge(), Forge::None);
        if is_pr && !has_forge {
            return Err(BaseError::new(
                400,
                URN_PR_REFS_UNSUPPORTED,
                "this repo has no forge remote to fetch PR refs from",
            ));
        }
        let access = if has_forge { Some(self.access()) } else { None };
        let access_ok = access.as_ref().and_then(|a| a.as_ref().ok());
        if is_pr {
            let head_branch = pr_head_branch.map(str::to_string);
            let chain = PrChain {
                explicit: Some(classified),
                forge_api: forge_base_ref.map(str::to_string),
                caller: None,
                head_branch: head_branch.clone(),
            };
            resolve_pr_base(&chain, || {
                self.default_branch(access_ok, head_branch.as_deref())
            })
        } else {
            let head_branch = branch_of_head(head_ref);
            let chain = NonPrChain {
                explicit: Some(classified),
                stack_parent: None,
                upstream: self.upstream_of(head_branch.as_deref(), &mapped),
                head_branch: head_branch.clone(),
                has_forge,
            };
            resolve_non_pr_base(&chain, || {
                self.default_branch(access_ok, head_branch.as_deref())
            })
        }
    }

    /// The `--base` grammar against this member + store (for `retrack`,
    /// RS-U7): `Ok(policy: None)` = `auto`. Blocking; may probe the forge
    /// (`ls-remote`) for a bare branch name nothing local knows.
    pub fn classify(&self, input: Option<&str>, is_pr: bool) -> Result<Classified, BaseError> {
        let probe = StoreProbe::new(self);
        classify_base(input, is_pr, &probe)
    }

    /// Persist a policy + status on review `id` (the whole policy: mode,
    /// branch, member, set_by). Callers that did NOT change the policy must
    /// write only the status (`Store::set_review_base_status`).
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
    ///
    /// Policy writes happen under the `ops` lock, and only when this call
    /// changed the policy (override / retarget / legacy upgrade) AND the
    /// stored policy is still the one it started from — a concurrent
    /// retrack or retarget always wins. Otherwise only `base_status` is
    /// written.
    ///
    /// A tracked base branch the forge no longer has: `set_by=auto` →
    /// re-resolved through the chain (kind `retarget`); a user/legacy base
    /// on an explicit fetch → 409 `base-vanished`; a not-yet-persisted
    /// legacy upgrade → evaluated verbatim as before. Always a
    /// `base-vanished` warning.
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
        let policy_key = |r: Option<&crate::store::ReviewBaseRow>| {
            r.map(|b| {
                (
                    b.base_mode.clone(),
                    b.base_branch.clone(),
                    b.base_member,
                    b.base_set_by.clone(),
                )
            })
        };
        let started_from = policy_key(base_row.as_ref());
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
        let mut warnings = class.warnings.clone();
        warnings.extend(rc.api_warnings.iter().cloned());
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
        let access = (rc.network && has_forge).then(|| self.access());
        let tracked = |e: &EffectiveBase| -> Vec<String> {
            e.policy()
                .filter(|p| p.mode == BaseMode::Track)
                .and_then(|p| p.branch.clone())
                .into_iter()
                .collect()
        };
        let mut fetch = match &access {
            Some(a) => {
                self.fetch_forge(a.as_ref().map_err(String::as_str), &tracked(&effective), pr)
            }
            None => FetchReport::cached(),
        };
        // A tracked base branch the forge no longer has.
        let mut vanished_branch = None;
        if let Some(p) = effective.policy().cloned() {
            if let Some(b) = p
                .branch
                .clone()
                .filter(|b| p.mode == BaseMode::Track && fetch.vanished.contains(b))
            {
                vanished_branch = Some(b.clone());
                warnings.push(warning(
                    warn::BASE_VANISHED,
                    format!("the base branch {b:?} no longer exists on the forge"),
                ));
                if class.upgraded && rc.policy_override.is_none() && !retargeted {
                    // A legacy row whose upgrade never persisted: keep its
                    // pre-upgrade behaviour, never persist onto a dead branch.
                    effective = EffectiveBase::Verbatim(review.base_ref.clone());
                } else if p.set_by == SetBy::Auto && rc.policy_override.is_none() {
                    let access_ok = access.as_ref().and_then(|a| a.as_ref().ok());
                    let head_branch = match pr {
                        Some(_) => None,
                        None => branch_of_head(&review.head_ref),
                    };
                    let api = rc
                        .forge_base_ref
                        .as_deref()
                        .filter(|a| valid_branch_name(a) && *a != b)
                        .map(|a| (a.to_string(), BaseSource::ForgeApi, vec![]));
                    let next = api.or_else(|| {
                        self.default_branch(access_ok, head_branch.as_deref())
                            .ok()
                            .filter(|(d, _)| *d != b)
                            .map(|(d, w)| (d, BaseSource::DefaultAssumed, w))
                    });
                    let Some((nb, src, w)) = next else {
                        return Err(BaseError::new(
                            409,
                            super::URN_BASE_VANISHED,
                            format!(
                                "the base branch {b:?} no longer exists on the forge and no other target could be resolved — pass --base <branch> (retrack)"
                            ),
                        ));
                    };
                    warnings.extend(w);
                    warnings.push(warning(
                        warn::RETARGETED,
                        format!("the base {b:?} vanished; the review now tracks {nb:?}"),
                    ));
                    effective = EffectiveBase::Policy(BasePolicy::track(&nb, SetBy::Auto, src));
                    kind_hint = Some(PatchsetKind::Retarget);
                    persist = true;
                    retargeted = true;
                    if let Some(a) = &access {
                        let again = self.fetch_forge(
                            a.as_ref().map_err(String::as_str),
                            std::slice::from_ref(&nb),
                            None,
                        );
                        if again.vanished.contains(&nb) || !again.fetched() {
                            return Err(BaseError::new(
                                409,
                                super::URN_BASE_VANISHED,
                                format!("the base {b:?} vanished and its replacement {nb:?} could not be fetched"),
                            ));
                        }
                        fetch.state = again.state;
                        fetch.code = again.code;
                    }
                } else {
                    return Err(BaseError::new(
                        409,
                        super::URN_BASE_VANISHED,
                        format!(
                            "the base branch {b:?} no longer exists on the forge — retrack the review: kb-code review retrack {} --base <branch>",
                            review.id
                        ),
                    ));
                }
            }
        }
        warnings.extend(fetch.warnings());
        if let Some(sha) = &fetch.pr_head {
            let _ = self.store.set_review_pr_head_sha(review.id, sha);
        }
        if class.upgraded
            && rc.policy_override.is_none()
            && !retargeted
            && vanished_branch.is_none()
            && fetch.fetched()
        {
            persist = true;
        }
        let mut status = status_after(&fetch, &effective, &prev_status);
        if vanished_branch.is_some() && retargeted {
            status.code = Some("base-vanished".into());
        }
        if rc
            .api_warnings
            .iter()
            .any(|w| w.code == warn::CREDENTIAL_ACCOUNT_MISMATCH)
        {
            status.code = Some(warn::CREDENTIAL_ACCOUNT_MISMATCH.into());
        }
        let status_json = status.to_json();
        let display = effective
            .policy()
            .map(|p| p.display_base_ref(mapped.first().map(String::as_str)));
        let id = review.id;
        let outcome = self.capture_with(
            review,
            &effective,
            &CaptureOpts {
                force: rc.force,
                kind_hint,
            },
            |_| {
                // Under the ops lock: re-read, then write.
                let now_row = self.store.get_review_base(id).ok().flatten();
                let unchanged = policy_key(now_row.as_ref()) == started_from;
                match (persist && unchanged, effective.policy()) {
                    (true, Some(p)) => {
                        self.persist_policy(id, p, &status);
                        if let Some(d) = &display {
                            let _ = self.store.set_review_base_ref(id, d);
                        }
                    }
                    _ => {
                        let _ = self.store.set_review_base_status(id, &status_json);
                    }
                }
            },
        )?;
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
