//! Read-only gix wrapper (W1.3) — repo handle, refs, tree reads, blob reads.
//!
//! Scope: object-database (ODB) reads only. The *working tree* on disk is
//! never consulted here — that is the watcher's job in a later wave (see the
//! module doc on `refs`/`tree`/`blob`). Every read takes a revspec and asks
//! gix to resolve it against the ODB, so results are identical whether or
//! not the working tree happens to be dirty.
//!
//! `GitRepo::open` canonicalizes nothing itself — callers (the config loader,
//! `kb-code.toml`'s `resolve_repos`) are expected to hand in an already
//! canonical path per invariant #27; `gix::discover` is what actually walks
//! up from that path to find the `.git` (file or directory — the linked-
//! worktree case) and resolves the gitdir/commondir split.
//!
//! `gix::Repository` is **not** `Send`/`Sync` (it carries internal caches),
//! so `GitRepo` inherits that. Fine for the CLI's single-threaded direct
//! calls; the daemon HTTP wiring (W1.6) will need to either reopen a
//! `GitRepo` per request (cheap — `gix::discover` is a handful of stats +
//! one config parse) or route through `ThreadSafeRepository`/
//! `spawn_blocking`. Not solved here on purpose — scope is the library API,
//! not its HTTP transport.

mod blob;
mod commit;
mod refs;
/// V70-A2 (SEC-17) — the validated revspec/range TYPES every git helper in
/// this crate takes instead of a bare `&str`. See that module's doc.
pub mod revspec;
mod tree;

pub use blob::DEFAULT_BLOB_SIZE_CAP;
pub use commit::CommitInfo;
pub use refs::{default_branch, RefInfo, RefKind};
pub use revspec::{RefRange, Revspec, RevspecError};
pub use tree::{EntryKind, TreeEntry};

use std::path::{Path, PathBuf};

/// Errors from the `git` module. Mirrors `kb_core::Error`'s convention of
/// carrying a rendered message rather than boxing gix's own (numerous,
/// deeply nested) error types — gix's `Display` impls are already
/// descriptive, and this keeps `GitError` trivially `Send + Sync + 'static`
/// regardless of what gix does internally.
#[derive(Debug, thiserror::Error)]
pub enum GitError {
    #[error("failed to open git repository at {path}: {message}")]
    Open { path: PathBuf, message: String },

    #[error("failed to resolve revision {spec:?}: {message}")]
    Resolve { spec: String, message: String },

    #[error("failed to read HEAD: {message}")]
    Head { message: String },

    #[error("failed to list references: {message}")]
    Refs { message: String },

    #[error("path not found at {rev:?}: {path:?}")]
    PathNotFound { rev: String, path: String },

    #[error("{path:?} at {rev:?} is a {actual}, not a directory")]
    NotADir {
        rev: String,
        path: String,
        actual: &'static str,
    },

    #[error("{path:?} at {rev:?} is a {actual}, not a readable file")]
    NotABlob {
        rev: String,
        path: String,
        actual: &'static str,
    },

    #[error("{path:?} at {rev:?} is {size} bytes, exceeding the {cap}-byte cap")]
    TooLarge {
        rev: String,
        path: String,
        size: u64,
        cap: u64,
    },

    #[error("git object database error: {message}")]
    Odb { message: String },
}

pub type Result<T> = std::result::Result<T, GitError>;

/// HEAD's state: born-and-symbolic (the common case), detached, or unborn
/// (a freshly `git init`'d repo with zero commits — `branch` is still
/// populated in that case, since `HEAD` nominally points at
/// `refs/heads/<default>` even before that ref exists).
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct HeadInfo {
    /// `true` if HEAD points directly at a commit rather than a branch.
    pub detached: bool,
    /// `true` if HEAD's branch doesn't exist yet (no commits).
    pub unborn: bool,
    /// Short branch name (e.g. `"main"`), if HEAD is symbolic (born or
    /// unborn). `None` only when HEAD is detached.
    pub branch: Option<String>,
    /// Full hex object id HEAD resolves to. `None` only when unborn.
    pub sha: Option<String>,
}

/// A read-only handle onto one git repository's object database, opened via
/// `gix::discover` at construction time. Cheap to hold; cheap to reopen.
pub struct GitRepo {
    pub(crate) repo: gix::Repository,
}

// `gix::Repository` itself doesn't implement `Debug` (it carries raw
// buffers/caches not worth dumping), so this is hand-rolled rather than
// derived — a public type should still be `Debug` for `assert`/`expect`
// ergonomics in callers and tests.
impl std::fmt::Debug for GitRepo {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("GitRepo")
            .field("git_dir", &self.repo.git_dir())
            .finish()
    }
}

impl GitRepo {
    /// Discover + open the repository containing (or at) `path`. Accepts a
    /// repo root, a subdirectory of one, or — the linked-worktree case — a
    /// worktree directory whose `.git` is a *file* pointing at
    /// `<main>/.git/worktrees/<name>` rather than a directory.
    pub fn open(path: &Path) -> Result<Self> {
        let repo = gix::discover(path).map_err(|e| GitError::Open {
            path: path.to_path_buf(),
            message: e.to_string(),
        })?;
        Ok(Self { repo })
    }

    /// `true` if this is a **linked** worktree (`git worktree add` output),
    /// i.e. the private per-worktree gitdir differs from the shared common
    /// gitdir. `false` for the main worktree of a normal repo (where the
    /// two coincide) and for a bare repository.
    pub fn is_worktree(&self) -> bool {
        self.repo.git_dir() != self.repo.common_dir()
    }

    /// This repository's private gitdir — for a linked worktree, the
    /// `<main>/.git/worktrees/<name>` directory; otherwise the same as
    /// `common_dir()`.
    pub fn git_dir(&self) -> &Path {
        self.repo.git_dir()
    }

    /// The shared gitdir all worktrees of this repository point at —
    /// objects/refs/config live here regardless of which worktree is open.
    pub fn common_dir(&self) -> &Path {
        self.repo.common_dir()
    }

    /// `true` if this repository is a shallow clone (`git clone --depth`),
    /// i.e. history is truncated at some boundary and ancestor walks past
    /// it will fail.
    pub fn is_shallow(&self) -> bool {
        self.repo.is_shallow()
    }

    /// Describe HEAD: detached vs. symbolic, born vs. unborn, branch name,
    /// resolved sha.
    pub fn head_info(&self) -> Result<HeadInfo> {
        let head = self.repo.head().map_err(|e| GitError::Head {
            message: e.to_string(),
        })?;
        let detached = head.is_detached();
        let unborn = head.is_unborn();
        let branch = head.referent_name().map(|n| n.shorten().to_string());
        let sha = head.id().map(|id| id.to_string());
        Ok(HeadInfo {
            detached,
            unborn,
            branch,
            sha,
        })
    }

    /// Resolve any git revspec — full sha, unambiguous short sha, branch
    /// name, tag name, `HEAD`, `HEAD~n`, `HEAD^`, etc — to an object id,
    /// exactly as `git rev-parse --verify <spec>` would.
    ///
    /// A shallow clone's ancestor walk can run off the shallow boundary
    /// (e.g. `HEAD~5` on a `--depth=1` clone); that surfaces as an
    /// ordinary resolution failure from gix (never a panic), and this
    /// appends an explicit hint to the error message when the repo is
    /// shallow so callers aren't left guessing why a well-formed spec
    /// didn't resolve.
    pub fn resolve(&self, spec: &str) -> Result<gix::ObjectId> {
        self.repo
            .rev_parse_single(spec)
            .map(Into::into)
            .map_err(|e| {
                let mut message = e.to_string();
                if self.repo.is_shallow() {
                    message.push_str(
                        " (repository is a shallow clone — the revision may be missing \
                     because it is behind the shallow boundary)",
                    );
                }
                GitError::Resolve {
                    spec: spec.to_string(),
                    message,
                }
            })
    }

    /// List local branches and tags: name, full ref name, resolved target
    /// sha, and whether it's the branch HEAD currently points at. Remote-
    /// tracking refs are **not** included — see [`Self::list_remote_branches`].
    pub fn list_refs(&self) -> Result<Vec<RefInfo>> {
        refs::list_refs(self)
    }

    /// List `refs/remotes/<remote>/<name>` (skipping each `<remote>/HEAD`
    /// symref). Short name is the full name minus `refs/remotes/<remote>/`.
    pub fn list_remote_branches(&self) -> Result<Vec<RefInfo>> {
        refs::list_remote_branches(self)
    }

    /// `refs/remotes/origin/HEAD` symbolic target, else [`Self::head_info`]'s
    /// branch. Shared by `/api/branches` and [`crate::reviews::default_base_ref`].
    pub fn default_branch(&self) -> Option<String> {
        refs::default_branch(self)
    }

    /// List the entries of a directory at `rev` (pass `""` or `"/"` for the
    /// repo root). Pure ODB read — the working tree is never consulted.
    /// Submodule entries are reported (kind, pinned sha) but never
    /// descended into.
    pub fn list_tree(&self, rev: &str, path: &str) -> Result<Vec<TreeEntry>> {
        tree::list_tree(self, rev, path)
    }

    /// Read the raw bytes of the blob at `path` within `rev`, refusing to
    /// load more than `max_bytes` (use `DEFAULT_BLOB_SIZE_CAP` for the
    /// library default of 10 MB). The size check is a cheap ODB header
    /// lookup, so an oversized blob's content is never actually loaded.
    pub fn read_blob(&self, rev: &str, path: &str, max_bytes: u64) -> Result<Vec<u8>> {
        blob::read_blob(self, rev, path, max_bytes)
    }

    /// The committed blob's object id for `path` at `rev`, or `None` if
    /// that path doesn't exist at that revision — see `blob::blob_oid`'s
    /// doc.
    pub fn blob_oid(&self, rev: &str, path: &str) -> Result<Option<String>> {
        blob::blob_oid(self, rev, path)
    }
}

#[cfg(test)]
mod tests;
