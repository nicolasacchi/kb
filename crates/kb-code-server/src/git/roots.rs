//! RS-U4 — the review-store TYPE SPLIT (design-internal-store.md §6,
//! README §9: "enforcement is by type").
//!
//! kb-code reads git from two kinds of place, and after the internal
//! review store lands they must never be confused:
//!
//! * [`WorkTreeRoot`] — a USER clone (or linked worktree) named in
//!   `kb-code.toml`. Index, watcher, blame/why/story, zeitwerk, complexity,
//!   fork-point, stacks, suggestion apply: everything that reads the user's
//!   files or reflog stays here.
//! * [`StoreRoot`] — a kb-owned bare store (`<state>/git/<uuid>.git`,
//!   README §5). Only ever built from a `review_stores` row.
//! * [`GitCtx`] — what a REVIEW/PR read runs against: the member work tree
//!   plus, when that repo's store is `ready`, the store. Until a store is
//!   ready every read falls back to the work tree — "exactly today's
//!   behaviour" (README §10.1) — and the fallback is COUNTED
//!   ([`GitFallbackStats`], read via `Store::git_fallback_stats`) so the
//!   Phase-1 gate "0 fallback hits after ready" has something to read.
//!
//! The git helpers that used to take `repo_root: &Path`
//! (`history::run_git_raw`, `reviews::run_git`, `reviews::files_changed`,
//! `review_comments::read_blob_text`, `routes::read_repo_file`) now take a
//! [`GitRoot`] / [`GitCtx`] / `routes::RevResolver` instead. [`GitRoot`] is
//! SEALED and there is no `From<&Path>` for any of these types, so a caller
//! that has not classified its root does not compile:
//!
//! ```compile_fail
//! // A raw path is not a classified root — this must not compile.
//! let _ = kb_code_server::reviews::files_changed(
//!     std::path::Path::new("."),
//!     "0000000000000000000000000000000000000000",
//!     "0000000000000000000000000000000000000000",
//! );
//! ```
//!
//! ```
//! // The classified form compiles (and, against a non-repo, simply errors).
//! use kb_code_server::git::roots::{GitCtx, WorkTreeRoot};
//! let ctx = GitCtx::work_tree_only(WorkTreeRoot::user_clone(std::env::temp_dir()));
//! let _ = kb_code_server::reviews::files_changed(
//!     &ctx,
//!     "0000000000000000000000000000000000000000",
//!     "0000000000000000000000000000000000000000",
//! );
//! ```
//!
//! `GitCtx` store resolution lives in exactly ONE place, the
//! [`GitCtx::for_repo`] constructor (and its async twin
//! [`GitCtx::resolve`]). With no `ready` store for the repo — not
//! registered, still seeding, broken, the member's own import pending, or
//! the whole store subsystem switched off for this boot — every `GitCtx`
//! resolves to the fallback and behaviour is exactly what it was before
//! this split. That last arm is enforced by the FIRST check in
//! `resolve_ready_store`, through `Store::review_store_readable` (a
//! boot-published flag on the `Store`): boot pushes
//! `StoreSettings::disabled` there, because this module sees only
//! `&Store`. The store half is LIVE: the boot job seeds
//! (`ReviewStores::seed` writes `state = "ready"`) and
//! `mark_imported` sets `legacy_import_json`, so a normally seeded daemon
//! resolves real store roots on the very next boot.
//!
//! Two sites read review data WITHOUT going through any of the typed
//! helpers — `prose_refs.rs` and `refs_typeahead.rs` open a `GitRepo` on
//! `Store::repo_root(id)` directly. A type cannot catch those; the
//! `review_store_bypass_tripwire` test in `tests/security/git_argv_lint.rs`
//! pins them instead.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use crate::config::RepoEntry;
use crate::store::{ReviewStoreRow, Store, StoreBlocking};

mod sealed {
    pub trait Sealed {}
}

/// A classified git root: something a git subprocess (`git -C <root>`) or
/// a `GitRepo::open` may run against. SEALED — only [`StoreRoot`] and
/// [`WorkTreeRoot`] implement it, and a `&Path` never does.
pub trait GitRoot: sealed::Sealed {
    /// The directory handed to `git -C` / `GitRepo::open`.
    fn git_path(&self) -> &Path;

    /// A read-only alternate object directory a subprocess against this
    /// root should see (`GIT_ALTERNATE_OBJECT_DIRECTORIES`, per process,
    /// never written to the repo's own `objects/info/alternates`). Only a
    /// [`BridgedWorkTree`] with a ready store carries one.
    fn alternate_objects(&self) -> Option<&Path> {
        None
    }
}

/// The env pair a git subprocess against `root` must carry — empty unless
/// `root` is a bridged work tree over a ready store. Spread with
/// `Command::envs`.
pub fn alternates_env(root: &dyn GitRoot) -> Option<(&'static str, &Path)> {
    root.alternate_objects()
        .map(|dir| ("GIT_ALTERNATE_OBJECT_DIRECTORIES", dir))
}

/// A kb-owned bare review store (README §5). Constructible only from a
/// `review_stores` row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoreRoot {
    git_dir: PathBuf,
}

impl StoreRoot {
    /// The store a `review_stores` row names. `pub(crate)`, not `pub`: a
    /// `StoreRoot` minted from a row bypasses `resolve_ready_store` —
    /// and with it the boot's read gate — entirely, so it must not be
    /// reachable from outside the crate. `from_handle` below is the same
    /// argument from the other side: a `StoreHandle` is itself only ever
    /// built from a row that already cleared the gate.
    pub(crate) fn from_row(row: &ReviewStoreRow) -> Self {
        Self {
            git_dir: PathBuf::from(&row.git_dir),
        }
    }

    pub fn git_dir(&self) -> &Path {
        &self.git_dir
    }

    /// RS-U6 — the store a ready [`crate::review_store::StoreHandle`]
    /// names (a handle is itself only ever built from a `review_stores`
    /// row, `ReviewStores::open`).
    pub fn from_handle(handle: &crate::review_store::StoreHandle) -> Self {
        Self {
            git_dir: handle.git_dir.clone(),
        }
    }

    /// `<store>/objects` — what the S8 bridges hand a USER-repo git
    /// invocation as a per-process, read-only
    /// `GIT_ALTERNATE_OBJECT_DIRECTORIES` (design §6 S8, blame).
    pub fn objects_dir(&self) -> PathBuf {
        self.git_dir.join("objects")
    }

    #[cfg(test)]
    pub(crate) fn for_test(git_dir: impl Into<PathBuf>) -> Self {
        Self {
            git_dir: git_dir.into(),
        }
    }
}

impl sealed::Sealed for StoreRoot {}
impl GitRoot for StoreRoot {
    fn git_path(&self) -> &Path {
        &self.git_dir
    }
}

/// A user clone (or linked worktree) — the repo a `kb-code.toml` entry
/// names.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkTreeRoot {
    path: PathBuf,
}

impl WorkTreeRoot {
    /// The work tree of a configured repo.
    pub fn of_repo(repo: &RepoEntry) -> Self {
        Self {
            path: repo.path.clone(),
        }
    }

    /// A path the CALLER vouches is a user clone (a configured repo's
    /// `path` threaded down as a bare `&Path`, `Store::repo_root(id)`, a
    /// test fixture). Deliberately a named constructor rather than
    /// `From<&Path>`: writing it IS the classification.
    pub fn user_clone(path: impl Into<PathBuf>) -> Self {
        Self { path: path.into() }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }
}

impl sealed::Sealed for WorkTreeRoot {}
impl GitRoot for WorkTreeRoot {
    fn git_path(&self) -> &Path {
        &self.path
    }
}

/// A USER work tree that can also SEE a ready review store's objects,
/// read-only (design §6 S8: blame/compare "with a per-process read-only
/// `GIT_ALTERNATE_OBJECT_DIRECTORIES=<store>/objects` on the *user*
/// invocation"). Names (`HEAD`, branches, reflog) still resolve in the
/// user repo; only object lookup widens. Built by
/// [`GitCtx::bridged_work_tree`]; with no ready store it is exactly the
/// work tree.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BridgedWorkTree {
    work: WorkTreeRoot,
    alternates: Option<PathBuf>,
}

impl BridgedWorkTree {
    pub fn work_tree(&self) -> &WorkTreeRoot {
        &self.work
    }
}

impl sealed::Sealed for BridgedWorkTree {}
impl GitRoot for BridgedWorkTree {
    fn git_path(&self) -> &Path {
        self.work.path()
    }
    fn alternate_objects(&self) -> Option<&Path> {
        self.alternates.as_deref()
    }
}

/// Per-`Store` counters of reads that did NOT come from a review store.
/// `unresolved`: a [`GitCtx`] was built for a repo with no `ready` store
/// (no row, seeding, broken, member import pending, or the store
/// subsystem off for this boot). `odb_miss`: a store WAS ready but a
/// content-addressed read had to be served by the user repo (the gate-3
/// number, README §14 (c)).
#[derive(Debug, Default)]
pub struct GitFallbackStats {
    unresolved: AtomicU64,
    odb_miss: AtomicU64,
}

/// A read-only snapshot of [`GitFallbackStats`].
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, serde::Serialize)]
pub struct GitFallbackSnapshot {
    pub unresolved: u64,
    pub odb_miss: u64,
}

impl GitFallbackStats {
    pub fn snapshot(&self) -> GitFallbackSnapshot {
        GitFallbackSnapshot {
            unresolved: self.unresolved.load(Ordering::Relaxed),
            odb_miss: self.odb_miss.load(Ordering::Relaxed),
        }
    }
}

/// What a review/PR read runs against — see the module doc.
#[derive(Debug, Clone)]
pub struct GitCtx {
    work: WorkTreeRoot,
    store: Option<StoreRoot>,
    stats: Arc<GitFallbackStats>,
}

impl GitCtx {
    /// THE store-resolution function (sync form — call it from a blocking
    /// context; async callers use [`Self::resolve`]). Looks the repo's
    /// store up by name (`Store::store_for_repo_name`) and uses it only
    /// when its state is `ready` AND this repo's own member import has
    /// landed; otherwise — no store, still seeding, broken, this member
    /// still `MemberPending`, or a DB error — falls back to `work` and
    /// counts the hit.
    pub fn for_repo(store: &Store, repo_name: &str, work: WorkTreeRoot) -> Self {
        let stats = store.git_fallbacks_handle();
        let resolved = Self::resolve_ready_store(store, repo_name);
        if resolved.is_none() {
            stats.unresolved.fetch_add(1, Ordering::Relaxed);
        }
        Self {
            work,
            store: resolved,
            stats,
        }
    }

    /// RS-U5 review fix — a `ready` STORE row is not enough: a member that
    /// just joined has its own `repo_stores.legacy_import_json` still
    /// unset until the background import lands (the same
    /// `MemberPending` case [`crate::review_store::registry::ReviewStores::
    /// handle_for_repo`] refuses on), meaning ITS reviews' refs may not be
    /// in the store yet even though the store itself is ready for OTHER
    /// members. This mirrors that check without needing a `ReviewStores`
    /// handle here — both read the same `repo_stores` row.
    fn resolve_ready_store(store: &Store, repo_name: &str) -> Option<StoreRoot> {
        // The whole store subsystem is switched OFF for this boot, so
        // EVERY write is already refused by `unavailable_reason` — but a
        // stale `ready` row (seeded before the operator moved/relative-ised
        // the root) survives the boot that disabled the store, because a
        // disabled boot returns before any row write. Without this check
        // the reads would keep serving a store root the daemon was
        // configured to refuse, and would hand `<store>/objects` to a
        // user-repo git invocation as alternates. The flag lives on the
        // `Store` precisely because this function sees only `&Store`.
        if !store.review_store_readable() {
            return None;
        }
        let row = match store.store_for_repo_name(repo_name) {
            Ok(Some(row)) if row.state == "ready" => row,
            Ok(_) => return None,
            Err(e) => {
                tracing::warn!(repo = repo_name, error = %e, "kb-code: review-store lookup failed; reading the work tree");
                return None;
            }
        };
        let repo_id = match store.repo_id(repo_name) {
            Ok(Some(id)) => id,
            Ok(None) => return None,
            Err(e) => {
                tracing::warn!(repo = repo_name, error = %e, "kb-code: repo id lookup failed; reading the work tree");
                return None;
            }
        };
        let pending = match store.repo_store(repo_id) {
            Ok(m) => m.is_some_and(|m| m.legacy_import_json.is_none()),
            Err(e) => {
                tracing::warn!(repo = repo_name, error = %e, "kb-code: member-import lookup failed; reading the work tree");
                return None;
            }
        };
        if pending {
            return None;
        }
        Some(StoreRoot::from_row(&row))
    }

    /// [`Self::for_repo`] for a configured repo entry.
    pub fn for_entry(store: &Store, repo: &RepoEntry) -> Self {
        Self::for_repo(store, &repo.name, WorkTreeRoot::of_repo(repo))
    }

    /// [`Self::for_repo`] from async context — the lookup is a `Store`
    /// query, so it rides `run_blocking` (store/mod.rs, 2026-08-31
    /// incident).
    pub async fn resolve(store: &Arc<Store>, repo_name: &str, work: WorkTreeRoot) -> Self {
        let name = repo_name.to_string();
        store
            .run_blocking(move |s| Self::for_repo(s, &name, work))
            .await
    }

    /// [`Self::resolve`] for a configured repo entry.
    pub async fn resolve_entry(store: &Arc<Store>, repo: &RepoEntry) -> Self {
        Self::resolve(store, &repo.name, WorkTreeRoot::of_repo(repo)).await
    }

    /// A context with no store and no counter — for callers that have no
    /// `Store` at all (the CLI's in-process paths, doc examples, unit
    /// fixtures). Reads behave exactly like the fallback.
    pub fn work_tree_only(work: WorkTreeRoot) -> Self {
        Self {
            work,
            store: None,
            stats: Arc::new(GitFallbackStats::default()),
        }
    }

    #[cfg(test)]
    pub(crate) fn with_store_for_test(
        work: WorkTreeRoot,
        store: StoreRoot,
        stats: Arc<GitFallbackStats>,
    ) -> Self {
        Self {
            work,
            store: Some(store),
            stats,
        }
    }

    pub fn work_tree(&self) -> &WorkTreeRoot {
        &self.work
    }

    pub fn store_root(&self) -> Option<&StoreRoot> {
        self.store.as_ref()
    }

    /// `true` when no ready store backs this context — the read falls back
    /// to the member work tree.
    pub fn is_fallback(&self) -> bool {
        self.store.is_none()
    }

    /// Where WRITES and name-scoped ref reads go: the store once ready,
    /// else the work tree. Never retried against the other root — a write
    /// that fails in the store must not land in the user repo.
    pub fn primary(&self) -> &dyn GitRoot {
        match &self.store {
            Some(s) => s,
            None => &self.work,
        }
    }

    /// Run a READ against the store first and, if that errs, the work tree
    /// (the `ReviewOdb` chain of design §6 S1: store, then user ODB). With
    /// no ready store this is exactly one call against the work tree. A
    /// store miss served by the work tree is counted as `odb_miss`.
    ///
    /// CONTENT-ADDRESSED READS ONLY — the chain is sound because a full
    /// sha means the same object in either ODB. A caller holding a rev
    /// NAME must not come here; use [`Self::read_rev_with_fallback`],
    /// which applies [`is_store_authoritative`].
    pub fn read_with_fallback<T, E>(
        &self,
        mut f: impl FnMut(&dyn GitRoot) -> Result<T, E>,
    ) -> Result<T, E> {
        if let Some(store) = &self.store {
            if let Ok(v) = f(store) {
                return Ok(v);
            }
            self.stats.odb_miss.fetch_add(1, Ordering::Relaxed);
        }
        f(&self.work)
    }

    /// Run a READ against the STORE ALONE — no retry, no `odb_miss`: once
    /// ready, the store is the whole answer for a `refs/kbc/*` name. With
    /// no ready store this is one call against the work tree, which is the
    /// only place such a ref can be (a pre-store install).
    pub fn read_store_only<T, E>(
        &self,
        mut f: impl FnMut(&dyn GitRoot) -> Result<T, E>,
    ) -> Result<T, E> {
        match &self.store {
            Some(store) => f(store),
            None => f(&self.work),
        }
    }

    /// [`Self::read_with_fallback`] for a caller-supplied `rev` that
    /// [`is_store_addressable`] admitted — the ONE place the
    /// store-vs-work-tree rule is applied to a name. A content-addressed
    /// sha takes the ODB chain; a `refs/kbc/*` NAME is
    /// store-authoritative ([`is_store_authoritative`]) and a store miss
    /// is returned as the error it is.
    pub fn read_rev_with_fallback<T, E>(
        &self,
        rev: &str,
        f: impl FnMut(&dyn GitRoot) -> Result<T, E>,
    ) -> Result<T, E> {
        if is_store_authoritative(rev) {
            self.read_store_only(f)
        } else {
            self.read_with_fallback(f)
        }
    }

    /// [`Self::read_with_fallback`] for `Option`-returning reads.
    pub fn read_opt_with_fallback<T>(
        &self,
        mut f: impl FnMut(&dyn GitRoot) -> Option<T>,
    ) -> Option<T> {
        self.read_with_fallback(|r| f(r).ok_or(())).ok()
    }

    /// The member work tree, widened (read-only) to the store's objects —
    /// see [`BridgedWorkTree`].
    pub fn bridged_work_tree(&self) -> BridgedWorkTree {
        BridgedWorkTree {
            work: self.work.clone(),
            alternates: self.alternates_for_work_tree(),
        }
    }

    /// The read-only alternate-objects dir a USER-repo invocation should
    /// carry so it can see store-only objects (design §6 S8 blame bridge).
    /// `None` while no store is ready — the invocation is then unchanged.
    pub fn alternates_for_work_tree(&self) -> Option<PathBuf> {
        self.store.as_ref().map(StoreRoot::objects_dir)
    }
}

/// `true` for a full 40-hex object id. A bridge may look a full sha up in
/// the store (objects are content-addressed, so either ODB answers the
/// same), but a NAME (`HEAD`, `main`, `HEAD~1`) means something different
/// in a bare store than in the user clone and must stay on the work tree.
pub fn is_object_id(rev: &str) -> bool {
    rev.len() == 40 && rev.bytes().all(|b| b.is_ascii_hexdigit())
}

/// `true` for a rev a bridged read should try in the review store before
/// the work tree: a full sha, or a kb-minted `refs/kbc/*` name.
pub fn is_store_addressable(rev: &str) -> bool {
    is_object_id(rev) || rev.starts_with("refs/kbc/")
}

/// `true` for a store-ADDRESSABLE rev the store is AUTHORITATIVE for: a
/// kb-minted `refs/kbc/*` name. Such a name names exactly one review, and
/// a user clone may still carry a STALE ref of the same name — pre-store
/// installs wrote `refs/kbc/*` into clones, and the clone-side copy
/// outlives a delete until `store legacy-refs` sweeps it. So a store miss
/// on a name is a miss, never a cue to read the clone: the same rule
/// `crate::checkout::resolve_target_via_store` applies. A full sha is
/// NOT authoritative (either ODB answers it identically), which is why
/// this is `addressable && !is_object_id` and not just "not an oid".
pub fn is_store_authoritative(rev: &str) -> bool {
    is_store_addressable(rev) && !is_object_id(rev)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::Command;

    fn git(dir: &Path, args: &[&str]) -> String {
        let out = Command::new("git")
            .arg("-C")
            .arg(dir)
            .args(args)
            .output()
            .expect("spawn git");
        assert!(out.status.success(), "git {args:?}: {out:?}");
        String::from_utf8(out.stdout).unwrap().trim().to_string()
    }

    fn fixture() -> (tempfile::TempDir, WorkTreeRoot, StoreRoot, String) {
        let tmp = tempfile::tempdir().unwrap();
        let work = tmp.path().join("work");
        let bare = tmp.path().join("store.git");
        std::fs::create_dir_all(&work).unwrap();
        git(&work, &["init", "-q"]);
        git(&work, &["config", "user.email", "t@example.com"]);
        git(&work, &["config", "user.name", "t"]);
        std::fs::write(work.join("a.txt"), "one\n").unwrap();
        git(&work, &["add", "a.txt"]);
        git(&work, &["commit", "-qm", "c1"]);
        let sha = git(&work, &["rev-parse", "HEAD"]);
        let st = Command::new("git")
            .args(["init", "-q", "--bare"])
            .arg(&bare)
            .status()
            .unwrap();
        assert!(st.success());
        (
            tmp,
            WorkTreeRoot::user_clone(work),
            StoreRoot::for_test(bare),
            sha,
        )
    }

    #[test]
    fn fallback_context_reads_the_work_tree_once() {
        let (_tmp, work, _store, sha) = fixture();
        let ctx = GitCtx::work_tree_only(work.clone());
        assert!(ctx.is_fallback());
        assert_eq!(ctx.primary().git_path(), work.path());
        let mut calls = Vec::new();
        let got = ctx.read_with_fallback(|r| {
            calls.push(r.git_path().to_path_buf());
            crate::history::run_git_raw(r, &["cat-file", "-t", &sha])
        });
        assert_eq!(got.unwrap(), b"commit\n");
        assert_eq!(calls, vec![work.path().to_path_buf()]);
        assert!(ctx.alternates_for_work_tree().is_none());
        let bridged = ctx.bridged_work_tree();
        assert!(alternates_env(&bridged).is_none());
        assert_eq!(bridged.git_path(), work.path());
    }

    #[test]
    fn ready_store_miss_falls_back_to_work_tree_and_is_counted() {
        let (_tmp, work, store, sha) = fixture();
        let stats = Arc::new(GitFallbackStats::default());
        let ctx = GitCtx::with_store_for_test(work, store.clone(), stats.clone());
        assert_eq!(ctx.primary().git_path(), store.git_dir());
        // The store is empty: the object is only in the user ODB.
        let got =
            ctx.read_with_fallback(|r| crate::history::run_git_raw(r, &["cat-file", "-t", &sha]));
        assert_eq!(got.unwrap(), b"commit\n");
        assert_eq!(stats.snapshot().odb_miss, 1);
        assert_eq!(
            ctx.alternates_for_work_tree(),
            Some(store.git_dir().join("objects"))
        );
    }

    #[test]
    fn bridged_work_tree_sees_store_only_objects_read_only() {
        let (_tmp, work, store, _sha) = fixture();
        // A commit that exists ONLY in the store.
        let scratch = work.path().parent().unwrap().join("scratch");
        std::fs::create_dir_all(&scratch).unwrap();
        git(&scratch, &["init", "-q"]);
        git(&scratch, &["config", "user.email", "t@example.com"]);
        git(&scratch, &["config", "user.name", "t"]);
        std::fs::write(scratch.join("b.txt"), "two\n").unwrap();
        git(&scratch, &["add", "b.txt"]);
        git(&scratch, &["commit", "-qm", "store-only"]);
        let only = git(&scratch, &["rev-parse", "HEAD"]);
        git(
            store.git_dir(),
            &[
                "fetch",
                "-q",
                scratch.to_str().unwrap(),
                "HEAD:refs/kbc/only",
            ],
        );
        let stats = Arc::new(GitFallbackStats::default());
        let ctx = GitCtx::with_store_for_test(work.clone(), store, stats);
        // Plain work tree: the object is missing.
        assert!(crate::history::run_git_raw(&work, &["cat-file", "-t", &only]).is_err());
        // Bridged: visible, and the user repo gained no alternates file.
        let bridged = ctx.bridged_work_tree();
        let got = crate::history::run_git_raw(&bridged, &["cat-file", "-t", &only]).unwrap();
        assert_eq!(got, b"commit\n");
        assert!(!work.path().join(".git/objects/info/alternates").exists());
    }

    #[test]
    fn ready_store_hit_does_not_count() {
        let (_tmp, work, store, sha) = fixture();
        // Seed the store with the commit (a plain local fetch).
        git(
            store.git_dir(),
            &[
                "fetch",
                "-q",
                work.path().to_str().unwrap(),
                "HEAD:refs/kbc/t",
            ],
        );
        let stats = Arc::new(GitFallbackStats::default());
        let ctx = GitCtx::with_store_for_test(work, store, stats.clone());
        let got =
            ctx.read_with_fallback(|r| crate::history::run_git_raw(r, &["cat-file", "-t", &sha]));
        assert_eq!(got.unwrap(), b"commit\n");
        assert_eq!(stats.snapshot().odb_miss, 0);
    }

    #[test]
    fn for_repo_without_a_store_row_falls_back_and_counts() {
        let tmp = tempfile::tempdir().unwrap();
        let store = Store::open(&tmp.path().join("kbc.sqlite")).unwrap();
        let ctx = GitCtx::for_repo(&store, "widgets", WorkTreeRoot::user_clone(tmp.path()));
        assert!(ctx.is_fallback());
        assert_eq!(store.git_fallback_stats().unresolved, 1);
    }

    /// RS-U5 review fix — a `ready` STORE row is not enough on its own: a
    /// member whose own import hasn't landed yet (`repo_stores.
    /// legacy_import_json` still unset) must fall back exactly like an
    /// absent store, mirroring `ReviewStores::handle_for_repo`'s
    /// `MemberPending` refusal.
    #[test]
    fn for_repo_falls_back_while_the_member_import_is_pending() {
        let tmp = tempfile::tempdir().unwrap();
        let store = Store::open(&tmp.path().join("kbc.sqlite")).unwrap();
        let repo_id = store.upsert_repo("widgets", "/nonexistent").unwrap();
        let store_id = store
            .create_review_store(
                "11111111-1111-4111-8111-111111111111",
                "github.com/acme/widgets",
                "/nonexistent.git",
                None,
                None,
                1,
            )
            .unwrap();
        store
            .set_review_store_state(store_id, "ready", None)
            .unwrap();
        // Leaves `legacy_import_json` at its column default (NULL, "not
        // imported yet") — exactly the state a member sits in between
        // joining and its background import landing.
        store.add_repo_to_store(repo_id, store_id).unwrap();
        let ctx = GitCtx::for_repo(&store, "widgets", WorkTreeRoot::user_clone(tmp.path()));
        assert!(
            ctx.is_fallback(),
            "a ready store with a still-pending member must fall back, not resolve"
        );
    }

    /// RS-U13 fix — a boot that DISABLES the store subsystem must make
    /// READS fall back, not only writes. The fixture is MAXIMALLY ready
    /// (a `ready` row over a real bare store, membership, and the import
    /// marker the MemberPending test above deliberately leaves NULL) so
    /// the only thing that CAN move resolution to the fallback is the
    /// boot-published flag — which is what the control assertion proves.
    #[test]
    fn for_repo_falls_back_when_the_store_subsystem_is_disabled() {
        let (tmp, work, store_root, sha) = fixture();
        let store = Store::open(&tmp.path().join("kbc.sqlite")).unwrap();
        let repo_id = store.upsert_repo("widgets", "/nonexistent").unwrap();
        let store_id = store
            .create_review_store(
                "11111111-1111-4111-8111-111111111111",
                "github.com/acme/widgets",
                store_root.git_dir().to_str().unwrap(),
                None,
                None,
                1,
            )
            .unwrap();
        store
            .set_review_store_state(store_id, "ready", None)
            .unwrap();
        store.add_repo_to_store(repo_id, store_id).unwrap();
        store
            .set_repo_store_legacy_import(repo_id, Some("{}"))
            .unwrap();

        // CONTROL: every input the read path wants is present, so this
        // MUST resolve the store — without it the rest proves nothing.
        let live = GitCtx::for_repo(&store, "widgets", work.clone());
        assert!(!live.is_fallback(), "control: a ready store must resolve");
        assert_eq!(
            live.store_root().map(|s| s.git_dir().to_path_buf()),
            Some(store_root.git_dir().to_path_buf())
        );
        assert_eq!(store.git_fallback_stats().unresolved, 0);

        store.set_review_store_readable(false);
        let ctx = GitCtx::for_repo(&store, "widgets", work.clone());
        assert!(
            ctx.is_fallback(),
            "a store disabled for this boot must not resolve a stale `ready` row"
        );
        assert!(ctx.store_root().is_none());
        // The CONSUMERS moved, not merely the flag: the work tree is
        // primary, `<store>/objects` is never handed to a user-repo git
        // invocation as alternates (SEC-13/15), and a store-only read
        // runs against the work tree.
        assert_eq!(ctx.primary().git_path(), work.path());
        assert!(ctx.alternates_for_work_tree().is_none());
        assert!(alternates_env(&ctx.bridged_work_tree()).is_none());
        let mut calls = Vec::new();
        let got = ctx.read_store_only(|r| {
            calls.push(r.git_path().to_path_buf());
            crate::history::run_git_raw(r, &["cat-file", "-t", &sha])
        });
        assert_eq!(got.unwrap(), b"commit\n");
        assert_eq!(calls, vec![work.path().to_path_buf()]);

        // A disabled resolution is a legitimate fallback: it is COUNTED,
        // not silently absorbed (the Phase-1 gate reads `unresolved`).
        assert_eq!(store.git_fallback_stats().unresolved, 1);

        // The registry half of the same verdict, in its own terms: this
        // is the predicate `bind_and_spawn` publishes onto the `Store`
        // above. (The write-side `admit_mutation` mapping predates this
        // change and is not re-pinned here.)
        let rs = crate::review_store::ReviewStores::disabled("root must be an absolute path");
        assert!(
            !rs.reads_can_use_store(),
            "a disabled registry must publish `readable = false`"
        );
    }

    #[test]
    fn store_addressable_revs() {
        assert!(is_store_addressable(&"a".repeat(40)));
        assert!(is_store_addressable("refs/kbc/review/1/ps2"));
        assert!(!is_store_addressable("HEAD"));
        assert!(!is_store_addressable("main"));
        assert!(!is_store_addressable(&"a".repeat(12)));
    }
}
