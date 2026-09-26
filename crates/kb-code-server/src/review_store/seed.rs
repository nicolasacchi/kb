//! RS-U3 — seeding a shared review store by FETCH (README §5.2, D3: no
//! hardlinks), plus the few read/sync primitives built on the same parts.
//!
//! [`seed_store`] is synchronous (every [`StoreGit`] call is) — run it
//! under `spawn_blocking`. It never writes into a member clone: the only
//! operations on a member are `for-each-ref`/`config --get-regexp` reads
//! with `GIT_DIR` pointed at its common dir, and the member acting as the
//! SOURCE of a local fetch (`upload-pack`).
//!
//! # Steps (README §5.2)
//!
//! 1. `git init --bare <root>/.seed-<uuid>.tmp`, then kb's store config
//!    (`gc.auto=0`, `maintenance.auto=false`, `fetch.prune=false`,
//!    `core.hooksPath=/dev/null`, `protocol.version=2`,
//!    `fetch.unpackLimit=1` (a fetch always lands as a pack, never a spray
//!    of loose objects on an IO-bound disk; U9's geometric repack merges
//!    the small ones),
//!    `uploadpack.allowAnySHA1InWant=true`, `HEAD -> refs/kbc/none`) and
//!    `pushurl = kbcode-no-push://refused` on every remote
//!    ([`StoreGit::configure_remote`]).
//! 2. Per member, a LOCAL no-credential fetch of its `refs/heads/*` into
//!    `refs/remotes/work-<repo_id>/*` (forced) and `refs/kbc/review/*` into
//!    itself (NOT forced: review ids are globally unique, so a clash is a
//!    real conflict and is reported, never overwritten). The refspec list
//!    is ENUMERATED from the member (`for-each-ref`) and fed on stdin —
//!    `StoreGit` never passes a wildcard refspec. Legacy `refs/kbc/pr/*` is
//!    NOT imported: it is a re-fetchable cache, and two clones may disagree
//!    on it (§15.2).
//! 3. Optionally, the base branches of open reviews from `base`, over the
//!    network, with the store's resolved credential. Offline-degradable:
//!    a failure is RECORDED ([`BaseFetch`]) and seeding continues.
//! 4. Connectivity: every patchset tip/base sha the DB knows for the
//!    members' reviews is checked (`cat-file --batch-check`). A missing tip
//!    is re-fetched BY SHA from each member (a review whose ref was lost but
//!    whose commit survives); whatever is still missing marks that review
//!    `objects-missing` — the store itself still goes `ready`. Present tips
//!    whose `refs/kbc/review/<id>/ps<n>` ref is absent get it recreated
//!    (create-only, README §5.4's integrity invariant); RS-U5 extends the
//!    same recreate to `refs/kbc/review/<id>/ps<n>-base` for every patchset
//!    whose `base_tip_sha` is non-NULL and present.
//! 5. Manifest written + fsynced, rename to `<uuid>.git`. ANY failure
//!    before the rename removes the `.tmp` immediately.

use std::collections::{BTreeMap, BTreeSet};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use serde::Serialize;

use super::classify::{classify, AuthContext, FailureClass};
use super::cred::FetchCredential;
use super::git::{GitArgs, GitCall, StoreGit, StoreGitError, BASE_FETCH_TIMEOUT};
use super::ladder::RemoteInfo;
use super::manifest::{self, Manifest};
use super::url::{FetchRefspec, RefName, RefSource, RemoteName, RemoteUrl};

/// Deadline for one member's seed fetch. A first local fetch copies the
/// whole object graph (measured in the RS-U3 benchmark); the 120 s
/// `WORK_FETCH_TIMEOUT` is for incremental work fetches, not this.
pub const SEED_FETCH_TIMEOUT: Duration = Duration::from_secs(60 * 60);
/// Cap on by-sha recovery attempts per seed (each is one local fetch).
const MAX_SHA_RECOVERY: usize = 64;

/// `objects_state` for a review whose commits exist nowhere reachable.
pub const OBJECTS_MISSING: &str = "objects-missing";

/// One member clone to seed from.
#[derive(Debug, Clone)]
pub struct SeedMember {
    pub repo_id: i64,
    /// The member's absolute `--git-common-dir` ([`common_dir_of`]).
    pub common_dir: PathBuf,
}

/// A patchset the DB knows (for the connectivity check).
#[derive(Debug, Clone)]
pub struct ExpectedPatchset {
    pub review_id: i64,
    pub ps_number: i64,
    pub tip_sha: String,
    pub base_sha: String,
    /// The base branch's tip at capture (V0045); `None` = legacy patchset.
    pub base_tip_sha: Option<String>,
}

/// What to seed.
#[derive(Debug, Clone)]
pub struct SeedPlan {
    pub root: PathBuf,
    pub uuid: String,
    pub store_key: String,
    /// The canonical transport URL of remote `base` (`None` for `local:`).
    pub base_url: Option<RemoteUrl>,
    pub members: Vec<SeedMember>,
    /// Base branches (bare names) of open reviews, for step 3.
    pub base_branches: Vec<String>,
    pub patchsets: Vec<ExpectedPatchset>,
    /// Test hook: fail right after this many members were fetched.
    #[doc(hidden)]
    pub fail_after_members: Option<usize>,
}

/// One member's import result.
#[derive(Debug, Clone, Serialize)]
pub struct MemberImport {
    pub repo_id: i64,
    pub heads: usize,
    pub review_refs: usize,
    /// Refs whose names are not valid store ref names (skipped, counted).
    pub skipped_refs: usize,
    /// Non-forced review refs the store already held at another commit.
    pub conflicts: Vec<String>,
    /// `refs/remotes/work-<id>/*` refs deleted because the member no
    /// longer has that branch (kb owns the namespace).
    pub pruned: usize,
}

/// Step 3's outcome.
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "state", rename_all = "kebab-case")]
pub enum BaseFetch {
    /// Not attempted (`code`: `offline-seed`, `no-base-remote`,
    /// `no-base-branches`, `no-credentials`, …).
    Skipped {
        code: String,
    },
    Fetched {
        branches: Vec<String>,
        vanished: Vec<String>,
    },
    Failed {
        code: String,
        detail: String,
    },
}

impl BaseFetch {
    pub fn code(&self) -> &str {
        match self {
            Self::Skipped { code } | Self::Failed { code, .. } => code,
            Self::Fetched { .. } => "fetched",
        }
    }
}

/// What seeding did.
#[derive(Debug, Clone, Serialize)]
pub struct SeedReport {
    pub git_dir: PathBuf,
    pub members: Vec<MemberImport>,
    pub base: BaseFetch,
    /// Review ids with at least one patchset commit missing.
    pub objects_missing: Vec<i64>,
    /// Review ids that had nothing missing (their `objects_state` clears).
    pub objects_ok: Vec<i64>,
    pub recovered_by_sha: usize,
    pub refs_recreated: usize,
    pub elapsed_ms: u128,
}

/// A seeding failure. `class` is a [`FailureClass`] slug; `detail` is
/// redacted (it comes from `StoreGitError`/io errors, never a URL).
#[derive(Debug, Clone, thiserror::Error, Serialize)]
#[error("seed failed at {stage} ({class}): {detail}")]
pub struct SeedError {
    pub stage: &'static str,
    pub class: &'static str,
    pub detail: String,
}

impl SeedError {
    fn git(stage: &'static str, e: StoreGitError) -> Self {
        Self {
            stage,
            class: e.class.slug(),
            detail: e.detail,
        }
    }
    fn io(stage: &'static str, e: std::io::Error) -> Self {
        let class = if e.raw_os_error() == Some(libc::ENOSPC) {
            FailureClass::DiskFull
        } else {
            FailureClass::Failed
        };
        Self {
            stage,
            class: class.slug(),
            detail: e.to_string(),
        }
    }
    fn other(stage: &'static str, detail: impl Into<String>) -> Self {
        Self {
            stage,
            class: FailureClass::Failed.slug(),
            detail: detail.into(),
        }
    }
}

/// The oldest git the store supports: `fetch --porcelain` (2.41).
pub const MIN_GIT: (u32, u32) = (2, 41);

/// Parse `git version X.Y[.Z…]` → `(X, Y)`.
pub fn parse_git_version(s: &str) -> Option<(u32, u32)> {
    let v = s.trim().strip_prefix("git version ")?;
    let mut it = v.split(|c: char| !c.is_ascii_digit());
    let major = it.next()?.parse().ok()?;
    let minor = it.next()?.parse().ok()?;
    Some((major, minor))
}

/// `Some(found)` when the store's git is older than `min` (or its version
/// cannot be read); `None` when it is new enough.
pub fn git_too_old(git: &StoreGit, min: (u32, u32)) -> Option<String> {
    let out = match git.run(GitCall::new("version", GitArgs::new("version"))) {
        Ok(o) => o,
        Err(e) => return Some(format!("unknown ({})", e.slug())),
    };
    let text = out.stdout_str().trim().to_string();
    match parse_git_version(&text) {
        Some(v) if v >= min => None,
        _ => Some(text.chars().take(64).collect()),
    }
}

/// `<root>/.seed-<uuid>.tmp`.
pub fn tmp_dir(root: &Path, uuid: &str) -> PathBuf {
    root.join(format!(".seed-{uuid}.tmp"))
}

/// `<root>/<uuid>.git`.
pub fn store_dir(root: &Path, uuid: &str) -> PathBuf {
    root.join(format!("{uuid}.git"))
}

/// The absolute git common dir of a working tree or bare repo at `path`,
/// resolved WITHOUT spawning git: `<path>/.git` (a dir, or a `gitdir:`
/// file for a linked worktree/submodule), followed by its `commondir`
/// file when present; a bare repo is its own common dir.
pub fn common_dir_of(path: &Path) -> std::io::Result<PathBuf> {
    let dotgit = path.join(".git");
    let gitdir = if dotgit.is_dir() {
        dotgit
    } else if dotgit.is_file() {
        let text = std::fs::read_to_string(&dotgit)?;
        let target = text
            .lines()
            .find_map(|l| l.strip_prefix("gitdir:"))
            .map(str::trim)
            .ok_or_else(|| std::io::Error::other(".git file has no gitdir line"))?;
        let t = Path::new(target);
        if t.is_absolute() {
            t.to_path_buf()
        } else {
            path.join(t)
        }
    } else if path.join("HEAD").is_file() && path.join("objects").is_dir() {
        path.to_path_buf()
    } else {
        return Err(std::io::Error::new(
            std::io::ErrorKind::NotFound,
            "not a git repository",
        ));
    };
    let common = match std::fs::read_to_string(gitdir.join("commondir")) {
        Ok(c) => {
            let c = Path::new(c.trim());
            if c.is_absolute() {
                c.to_path_buf()
            } else {
                gitdir.join(c)
            }
        }
        Err(_) => gitdir,
    };
    std::fs::canonicalize(common)
}

/// A member clone's remotes (`remote.<name>.url` / `.gh-resolved`), read
/// from its own config. Read-only on the clone.
pub fn read_remotes(git: &StoreGit, common_dir: &Path) -> Result<Vec<RemoteInfo>, StoreGitError> {
    let out = git.run(
        GitCall::new(
            "config",
            GitArgs::new("config")
                .flag("--get-regexp")
                .end_of_options()
                .flag(r"^remote\..*\.(url|gh-resolved)$"),
        )
        .git_dir(common_dir)
        .stdout_cap(1024 * 1024)
        .allow_nonzero(),
    )?;
    let mut by_name: BTreeMap<String, RemoteInfo> = BTreeMap::new();
    for line in out.stdout_str().lines() {
        let Some((k, v)) = line.split_once(' ') else {
            continue;
        };
        let Some(rest) = k.strip_prefix("remote.") else {
            continue;
        };
        let Some((name, var)) = rest.rsplit_once('.') else {
            continue;
        };
        let e = by_name
            .entry(name.to_string())
            .or_insert_with(|| RemoteInfo {
                name: name.to_string(),
                url: String::new(),
                gh_resolved: None,
            });
        match var {
            // First url wins (git's own rule for multiple `url` entries).
            "url" if e.url.is_empty() => e.url = v.to_string(),
            "gh-resolved" => e.gh_resolved = Some(v.to_string()),
            _ => {}
        }
    }
    Ok(by_name
        .into_values()
        .filter(|r| !r.url.is_empty())
        .collect())
}

/// `(oid, refname)` for `prefixes` in the repo at `git_dir`.
pub fn list_refs(
    git: &StoreGit,
    git_dir: &Path,
    prefixes: &[&'static str],
) -> Result<Vec<(String, String)>, StoreGitError> {
    let mut args = GitArgs::new("for-each-ref").flag("--format=%(objectname) %(refname)");
    if !prefixes.is_empty() {
        args = args.end_of_options();
        for p in prefixes {
            args = args.flag(p);
        }
    }
    let out = git.run(
        GitCall::new("for-each-ref", args)
            .git_dir(git_dir)
            .stdout_cap(256 * 1024 * 1024),
    )?;
    Ok(out
        .stdout_str()
        .lines()
        .filter_map(|l| {
            let (o, r) = l.split_once(' ')?;
            Some((o.to_string(), r.to_string()))
        })
        .collect())
}

/// Is `name` a review patchset ref (`refs/kbc/review/<id>/ps<n>[-base]`)?
pub fn is_review_ref(name: &str) -> bool {
    let Some(rest) = name.strip_prefix("refs/kbc/review/") else {
        return false;
    };
    let Some((id, ps)) = rest.split_once('/') else {
        return false;
    };
    let digits = |s: &str| !s.is_empty() && s.bytes().all(|c| c.is_ascii_digit());
    let ps = ps.strip_suffix("-base").unwrap_or(ps);
    digits(id) && ps.strip_prefix("ps").is_some_and(digits)
}

/// `refs/kbc/review/<id>/ps<n>`.
pub fn patchset_ref(review_id: i64, ps_number: i64) -> String {
    format!("refs/kbc/review/{review_id}/ps{ps_number}")
}

/// `refs/kbc/review/<id>/ps<n>-base` — the patchset's base-branch tip pin
/// (README §5.4/§8: `base_tip_sha` is not necessarily an ancestor of the
/// tip, so it needs its own keep-alive ref, same integrity invariant as
/// [`patchset_ref`]). RS-U5.
pub fn patchset_base_ref(review_id: i64, ps_number: i64) -> String {
    format!("refs/kbc/review/{review_id}/ps{ps_number}-base")
}

fn full_hex(s: &str) -> bool {
    (s.len() == 40 || s.len() == 64) && s.bytes().all(|c| matches!(c, b'0'..=b'9' | b'a'..=b'f'))
}

/// Write kb's store config into a freshly initialised bare repo.
fn write_store_config(dir: &Path, uuid: &str) -> std::io::Result<()> {
    let mut f = std::fs::OpenOptions::new()
        .append(true)
        .open(dir.join("config"))?;
    // `uuid` is kb-minted (hex + dashes); nothing else here is dynamic.
    write!(
        f,
        "[core]\n\tlogAllRefUpdates = always\n\thooksPath = /dev/null\n\
         [gc]\n\tauto = 0\n\
         [maintenance]\n\tauto = false\n\
         [fetch]\n\tprune = false\n\tunpackLimit = 1\n\
         [protocol]\n\tversion = 2\n\
         [uploadpack]\n\tallowAnySHA1InWant = true\n\
         [kbcode]\n\tstoreVersion = 1\n\tstoreUuid = {uuid}\n"
    )?;
    f.sync_all()?;
    std::fs::write(dir.join("HEAD"), "ref: refs/kbc/none\n")
}

/// Fetch refspecs on stdin (`fetch --stdin --porcelain`). Returns the
/// refs the fetch REJECTED (non-forced destination held elsewhere).
fn fetch_stdin(
    git: &StoreGit,
    git_dir: &Path,
    remote: &RemoteName,
    refspecs: &[FetchRefspec],
    timeout: Duration,
) -> Result<Vec<String>, StoreGitError> {
    if refspecs.is_empty() {
        return Ok(vec![]);
    }
    let mut input = String::new();
    for r in refspecs {
        input.push_str(&r.as_arg());
        input.push('\n');
    }
    let args = GitArgs::new("fetch")
        .flag("--no-tags")
        .flag("--no-write-fetch-head")
        .flag("--no-auto-gc")
        .flag("--no-auto-maintenance")
        .flag("--porcelain")
        .flag("--stdin")
        .end_of_options()
        .remote(remote);
    let out = git.run(
        GitCall::new("fetch", args)
            .git_dir(git_dir)
            .stdin(input.into_bytes())
            .timeout(timeout)
            .allow_nonzero(),
    )?;
    let rejected: Vec<String> = out
        .stdout_str()
        .lines()
        .filter(|l| l.starts_with('!'))
        .filter_map(|l| l.rsplit(' ').next().map(str::to_string))
        .collect();
    // Only a non-forced REVIEW ref may be rejected as a (reported)
    // conflict; any other rejection — a forced `work-<id>` ref that could
    // not be written, e.g. a D/F clash — is a real failure, never success.
    let (conflicts, other): (Vec<String>, Vec<String>) = rejected
        .into_iter()
        .partition(|r| r.starts_with("refs/kbc/review/"));
    if !other.is_empty() {
        return Err(StoreGitError {
            op: "fetch",
            class: FailureClass::Failed,
            exit_code: out.exit_code,
            detail: format!(
                "rejected {} ref(s), first {}: {}",
                other.len(),
                other[0],
                out.stderr.chars().take(1024).collect::<String>()
            ),
        });
    }
    if out.exit_code != Some(0) && conflicts.is_empty() {
        return Err(StoreGitError {
            op: "fetch",
            class: classify(&out.stderr, AuthContext::None),
            exit_code: out.exit_code,
            detail: out.stderr.chars().take(2048).collect(),
        });
    }
    Ok(conflicts)
}

/// Import one member into the store (step 2). Also the `work-<id>` sync
/// primitive for an already-seeded store (`store sync`).
pub fn import_member(
    git: &StoreGit,
    git_dir: &Path,
    member: &SeedMember,
    timeout: Duration,
) -> Result<MemberImport, StoreGitError> {
    git.allow_local_source(&member.common_dir)
        .map_err(|e| StoreGitError {
            op: "safe.directory",
            class: FailureClass::Failed,
            exit_code: None,
            detail: e.to_string(),
        })?;
    let remote = RemoteName::work(member.repo_id);
    let url = RemoteUrl::local_seed(&member.common_dir).map_err(|e| StoreGitError {
        op: "configure-remote",
        class: FailureClass::UrlRejected,
        exit_code: None,
        detail: e.to_string(),
    })?;
    git.configure_remote(git_dir, &remote, &url)?;
    let work_prefix = format!("refs/remotes/{}/", remote.as_str());
    // A ref deleted between listing and fetching fails the whole fetch
    // (`couldn't find remote ref`); re-list once and retry.
    let mut attempt = 0;
    loop {
        attempt += 1;
        let refs = list_refs(
            git,
            &member.common_dir,
            &["refs/heads/", "refs/kbc/review/"],
        )?;
        let mut specs = Vec::with_capacity(refs.len());
        let (mut heads, mut reviews, mut skipped) = (0, 0, 0);
        for (_, name) in &refs {
            if let Some(branch) = name.strip_prefix("refs/heads/") {
                match (
                    RefName::parse(name),
                    RefName::parse(&format!("{work_prefix}{branch}")),
                ) {
                    (Ok(src), Ok(dst)) => {
                        specs.push(FetchRefspec::new(true, RefSource::Ref(src), dst));
                        heads += 1;
                    }
                    _ => skipped += 1,
                }
            } else if is_review_ref(name) {
                match RefName::parse(name) {
                    Ok(r) => {
                        specs.push(FetchRefspec::new(false, RefSource::Ref(r.clone()), r));
                        reviews += 1;
                    }
                    Err(_) => skipped += 1,
                }
            } else {
                skipped += 1;
            }
        }
        // Prune BEFORE fetching: a branch renamed `feature` → `feature/x`
        // would otherwise D/F-clash with the stale `work-<id>/feature`.
        let wanted: BTreeSet<&str> = specs.iter().map(|s| s.dst().as_str()).collect();
        let pruned = prune_work_refs(git, git_dir, &work_prefix, &wanted)?;
        match fetch_stdin(git, git_dir, &remote, &specs, timeout) {
            Ok(conflicts) => {
                return Ok(MemberImport {
                    repo_id: member.repo_id,
                    heads,
                    review_refs: reviews,
                    skipped_refs: skipped,
                    conflicts,
                    pruned,
                })
            }
            Err(e) if e.class == FailureClass::Vanished && attempt < 2 => continue,
            Err(e) => return Err(e),
        }
    }
}

/// Delete store refs under `work_prefix` (`refs/remotes/work-<id>/`) that
/// are not in `wanted`. kb owns that namespace, so this never touches a
/// ref anything else wrote. One `update-ref --stdin` transaction, each
/// delete guarded by the old value.
///
/// A MIRROR PRUNE, not a GC, and the difference is load-bearing for the
/// cruft cooldown: this runs on every seed/sync/import with no dry run and
/// no pre-apply bundle, so it orphans objects without moving
/// `state_json.last_gc_apply` — and that timestamp is the only thing
/// `super::maint`'s monthly expiring-cruft repack consults. See the
/// cooldown's recorded scope note there. The behaviour is deliberately not
/// changed here: kb owns this namespace, the prune is what keeps the mirror
/// from growing without bound, and it is recoverable by re-fetching.
fn prune_work_refs(
    git: &StoreGit,
    git_dir: &Path,
    work_prefix: &str,
    wanted: &BTreeSet<&str>,
) -> Result<usize, StoreGitError> {
    let mut tx = String::new();
    let mut n = 0;
    for (oid, name) in list_refs(git, git_dir, &["refs/remotes/"])? {
        if name.starts_with(work_prefix) && !wanted.contains(name.as_str()) && full_hex(&oid) {
            if RefName::parse(&name).is_err() {
                continue;
            }
            tx.push_str(&format!("delete {name} {oid}\n"));
            n += 1;
        }
    }
    if n > 0 {
        git.run(
            GitCall::new("update-ref", GitArgs::new("update-ref").flag("--stdin"))
                .git_dir(git_dir)
                .stdin(tx.into_bytes()),
        )?;
    }
    Ok(n)
}

/// Step 3 / `store sync`: fetch `branches` from remote `base` into
/// `refs/remotes/base/<B>`. A branch the forge no longer has is reported
/// in `vanished`, and the rest are still fetched.
pub fn fetch_base_branches(
    git: &StoreGit,
    git_dir: &Path,
    branches: &[String],
    cred: &FetchCredential,
) -> BaseFetch {
    let Some(auth) = cred.auth() else {
        return BaseFetch::Skipped {
            code: FailureClass::NoCredentials.slug().into(),
        };
    };
    let mut specs = Vec::new();
    let mut names = Vec::new();
    for b in branches {
        if let (Ok(src), Ok(dst)) = (
            RefName::branch(b),
            RefName::parse(&format!("refs/remotes/base/{b}")),
        ) {
            specs.push(FetchRefspec::new(true, RefSource::Ref(src), dst));
            names.push(b.clone());
        }
    }
    if specs.is_empty() {
        return BaseFetch::Skipped {
            code: "no-base-branches".into(),
        };
    }
    let base = RemoteName::base();
    match git.fetch(git_dir, &base, &specs, auth, BASE_FETCH_TIMEOUT) {
        Ok(_) => BaseFetch::Fetched {
            branches: names,
            vanished: vec![],
        },
        Err(e) if e.class == FailureClass::Vanished => {
            // One by one, so a deleted base branch does not block the rest.
            let (mut ok, mut gone) = (Vec::new(), Vec::new());
            for (spec, name) in specs.iter().zip(names) {
                match git.fetch(
                    git_dir,
                    &base,
                    std::slice::from_ref(spec),
                    auth,
                    BASE_FETCH_TIMEOUT,
                ) {
                    Ok(_) => ok.push(name),
                    Err(e) if e.class == FailureClass::Vanished => gone.push(name),
                    Err(e) => {
                        return BaseFetch::Failed {
                            code: e.class.slug().into(),
                            detail: e.detail,
                        }
                    }
                }
            }
            BaseFetch::Fetched {
                branches: ok,
                vanished: gone,
            }
        }
        Err(e) => BaseFetch::Failed {
            code: e.class.slug().into(),
            detail: e.detail,
        },
    }
}

/// The subset of `shas` the repo at `git_dir` does not have.
pub fn missing_objects(
    git: &StoreGit,
    git_dir: &Path,
    shas: &BTreeSet<String>,
) -> Result<BTreeSet<String>, StoreGitError> {
    if shas.is_empty() {
        return Ok(BTreeSet::new());
    }
    let mut input = String::new();
    for s in shas {
        input.push_str(s);
        input.push('\n');
    }
    let out = git.run(
        GitCall::new("cat-file", GitArgs::new("cat-file").flag("--batch-check"))
            .git_dir(git_dir)
            .stdin(input.into_bytes()),
    )?;
    Ok(out
        .stdout_str()
        .lines()
        .filter_map(|l| l.strip_suffix(" missing"))
        .map(str::to_string)
        .collect())
}

/// Try to fetch `sha` from a member BY OBJECT ID into `dst` (create-only).
fn fetch_by_sha(git: &StoreGit, git_dir: &Path, member: &SeedMember, sha: &str, dst: &str) -> bool {
    let (Ok(src), Ok(dst)) = (RefSource::oid(sha), RefName::parse(dst)) else {
        return false;
    };
    let spec = FetchRefspec::new(false, src, dst);
    let args = GitArgs::new("fetch")
        .flag("--no-tags")
        .flag("--no-write-fetch-head")
        .flag("--no-auto-gc")
        .flag("--no-auto-maintenance")
        .flag("--quiet")
        // Per-invocation override on the SOURCE side; nothing is written to
        // the member's config (design §3.4).
        .flag("--upload-pack=git -c uploadpack.allowAnySHA1InWant=true upload-pack")
        .end_of_options()
        .remote(&RemoteName::work(member.repo_id))
        .refspec(&spec);
    git.run(
        GitCall::new("fetch", args)
            .git_dir(git_dir)
            .timeout(super::git::WORK_FETCH_TIMEOUT),
    )
    .is_ok()
}

/// Step 4 over an existing store: check every expected patchset, try the
/// by-sha recovery, recreate absent ps refs. Returns
/// `(objects_missing, objects_ok, recovered, refs_recreated)`.
pub fn verify_connectivity(
    git: &StoreGit,
    git_dir: &Path,
    members: &[SeedMember],
    patchsets: &[ExpectedPatchset],
    ops: Option<&tokio::sync::Mutex<()>>,
) -> Result<(Vec<i64>, Vec<i64>, usize, usize), StoreGitError> {
    let mut shas = BTreeSet::new();
    for p in patchsets {
        for s in [Some(&p.tip_sha), Some(&p.base_sha), p.base_tip_sha.as_ref()]
            .into_iter()
            .flatten()
        {
            if full_hex(s) {
                shas.insert(s.clone());
            }
        }
    }
    let mut missing = missing_objects(git, git_dir, &shas)?;
    let mut recovered = 0;
    let mut attempts = 0;
    for p in patchsets {
        if !missing.contains(&p.tip_sha) || attempts >= MAX_SHA_RECOVERY {
            continue;
        }
        for m in members {
            attempts += 1;
            if fetch_by_sha(
                git,
                git_dir,
                m,
                &p.tip_sha,
                &patchset_ref(p.review_id, p.ps_number),
            ) {
                recovered += 1;
                break;
            }
        }
    }
    if recovered > 0 {
        missing = missing_objects(git, git_dir, &shas)?;
    }
    // Recreate absent ps/ps-base refs whose sha is present (create-only),
    // under the store's ops lock (a capture writes the same ref family).
    // Taken with `blocking_lock`: callers are on a blocking thread.
    let _ops = ops.map(|m| m.blocking_lock());
    let have: BTreeSet<String> = list_refs(git, git_dir, &["refs/kbc/review/"])?
        .into_iter()
        .map(|(_, r)| r)
        .collect();
    let mut tx = String::new();
    let mut recreated = 0;
    for p in patchsets {
        let r = patchset_ref(p.review_id, p.ps_number);
        if full_hex(&p.tip_sha) && !missing.contains(&p.tip_sha) && !have.contains(&r) {
            tx.push_str(&format!("create {r} {}\n", p.tip_sha));
            recreated += 1;
        }
        // RS-U5 — the `-base` pin (README §5.4/§8): recreated only when
        // `base_tip_sha` is set AND its object is present. A patchset with
        // no `base_tip_sha` (legacy row, or a `pin` review that never had
        // one) gets no `-base` ref, same as today.
        if let Some(bt) = &p.base_tip_sha {
            let rb = patchset_base_ref(p.review_id, p.ps_number);
            if full_hex(bt) && !missing.contains(bt) && !have.contains(&rb) {
                tx.push_str(&format!("create {rb} {bt}\n"));
                recreated += 1;
            }
        }
    }
    if recreated > 0 {
        git.run(
            GitCall::new("update-ref", GitArgs::new("update-ref").flag("--stdin"))
                .git_dir(git_dir)
                .stdin(tx.into_bytes()),
        )?;
    }
    let mut bad = BTreeSet::new();
    let mut all = BTreeSet::new();
    for p in patchsets {
        all.insert(p.review_id);
        let tip_bad = !full_hex(&p.tip_sha) || missing.contains(&p.tip_sha);
        let base_bad = full_hex(&p.base_sha) && missing.contains(&p.base_sha);
        let base_tip_bad = p
            .base_tip_sha
            .as_ref()
            .is_some_and(|b| full_hex(b) && missing.contains(b));
        if tip_bad || base_bad || base_tip_bad {
            bad.insert(p.review_id);
        }
    }
    let ok = all.difference(&bad).copied().collect();
    Ok((bad.into_iter().collect(), ok, recovered, recreated))
}

/// Seed a store. See the module doc. `base_cred = None` skips step 3
/// (the boot job's local-only seed, D4) and records `offline-seed`.
pub fn seed_store(
    git: &StoreGit,
    plan: &SeedPlan,
    base_cred: Option<&FetchCredential>,
    now: i64,
) -> Result<SeedReport, SeedError> {
    let started = Instant::now();
    std::fs::create_dir_all(&plan.root).map_err(|e| SeedError::io("root", e))?;
    let tmp = tmp_dir(&plan.root, &plan.uuid);
    let fin = store_dir(&plan.root, &plan.uuid);
    if fin.exists() {
        return Err(SeedError::other(
            "rename",
            "the store directory already exists",
        ));
    }
    if tmp.exists() {
        // A crashed earlier attempt; the caller holds the store lock.
        std::fs::remove_dir_all(&tmp).map_err(|e| SeedError::io("cleanup", e))?;
    }
    let res = seed_into(git, plan, base_cred, &tmp, now);
    match res {
        Ok((members, base, missing, ok, recovered, recreated)) => {
            if let Err(e) = std::fs::rename(&tmp, &fin) {
                let _ = std::fs::remove_dir_all(&tmp);
                return Err(SeedError::io("rename", e));
            }
            Ok(SeedReport {
                git_dir: fin,
                members,
                base,
                objects_missing: missing,
                objects_ok: ok,
                recovered_by_sha: recovered,
                refs_recreated: recreated,
                elapsed_ms: started.elapsed().as_millis(),
            })
        }
        Err(e) => {
            let _ = std::fs::remove_dir_all(&tmp);
            Err(e)
        }
    }
}

type SeedParts = (
    Vec<MemberImport>,
    BaseFetch,
    Vec<i64>,
    Vec<i64>,
    usize,
    usize,
);

fn seed_into(
    git: &StoreGit,
    plan: &SeedPlan,
    base_cred: Option<&FetchCredential>,
    tmp: &Path,
    now: i64,
) -> Result<SeedParts, SeedError> {
    git.init_bare(tmp).map_err(|e| SeedError::git("init", e))?;
    write_store_config(tmp, &plan.uuid).map_err(|e| SeedError::io("config", e))?;
    if let Some(url) = &plan.base_url {
        git.configure_remote(tmp, &RemoteName::base(), url)
            .map_err(|e| SeedError::git("config", e))?;
    }
    let mut members = Vec::with_capacity(plan.members.len());
    for (i, m) in plan.members.iter().enumerate() {
        if plan.fail_after_members == Some(i) {
            return Err(SeedError::other("member-fetch", "injected failure"));
        }
        let imp = import_member(git, tmp, m, SEED_FETCH_TIMEOUT)
            .map_err(|e| SeedError::git("member-fetch", e))?;
        members.push(imp);
    }
    let base = match (base_cred, &plan.base_url) {
        (_, None) => BaseFetch::Skipped {
            code: "no-base-remote".into(),
        },
        (None, Some(_)) => BaseFetch::Skipped {
            code: "offline-seed".into(),
        },
        (Some(c), Some(_)) => fetch_base_branches(git, tmp, &plan.base_branches, c),
    };
    let (missing, ok, recovered, recreated) =
        verify_connectivity(git, tmp, &plan.members, &plan.patchsets, None)
            .map_err(|e| SeedError::git("connectivity", e))?;
    manifest::write(tmp, &Manifest::new(&plan.uuid, &plan.store_key, now))
        .map_err(|e| SeedError::io("manifest", e))?;
    Ok((members, base, missing, ok, recovered, recreated))
}

/// Remove `.seed-<uuid>.tmp` leftovers whose store lock is free (a crash
/// mid-seed). A tmp whose lock another process holds is being seeded right
/// now and is left alone. Returns how many were removed.
pub fn sweep_stale_tmp(root: &Path) -> usize {
    let Ok(rd) = std::fs::read_dir(root) else {
        return 0;
    };
    let mut n = 0;
    for e in rd.flatten() {
        let name = e.file_name();
        let Some(uuid) = name
            .to_str()
            .and_then(|s| s.strip_prefix(".seed-"))
            .and_then(|s| s.strip_suffix(".tmp"))
        else {
            continue;
        };
        if let Ok(Some(_lock)) = manifest::StoreLock::try_acquire(root, uuid) {
            if std::fs::remove_dir_all(e.path()).is_ok() {
                n += 1;
            }
        }
    }
    n
}

/// Disk facts for `store show` and the benchmark.
#[derive(Debug, Clone, Default, Serialize)]
pub struct StoreStats {
    pub packs: usize,
    pub pack_bytes: u64,
    pub loose_objects: usize,
    pub total_bytes: u64,
}

/// Walk a store directory. Pure fs; cheap enough for a status read.
pub fn store_stats(dir: &Path) -> StoreStats {
    let mut s = StoreStats::default();
    if let Ok(rd) = std::fs::read_dir(dir.join("objects/pack")) {
        for e in rd.flatten() {
            let p = e.path();
            let len = e.metadata().map(|m| m.len()).unwrap_or(0);
            if p.extension().and_then(|x| x.to_str()) == Some("pack") {
                s.packs += 1;
                s.pack_bytes += len;
            }
        }
    }
    if let Ok(rd) = std::fs::read_dir(dir.join("objects")) {
        for e in rd.flatten() {
            let n = e.file_name();
            let n = n.to_string_lossy();
            if n.len() == 2 && n.bytes().all(|c| c.is_ascii_hexdigit()) {
                s.loose_objects += std::fs::read_dir(e.path()).map(|r| r.count()).unwrap_or(0);
            }
        }
    }
    s.total_bytes = du(dir);
    s
}

fn du(p: &Path) -> u64 {
    let Ok(md) = std::fs::symlink_metadata(p) else {
        return 0;
    };
    if md.is_dir() {
        std::fs::read_dir(p)
            .map(|rd| rd.flatten().map(|e| du(&e.path())).sum())
            .unwrap_or(0)
    } else {
        md.len()
    }
}

#[cfg(test)]
#[path = "seed/tests.rs"]
mod tests;
