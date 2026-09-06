//! Sub-step (c) — reconcile, never replay. Two inputs feed one
//! `full_reconcile` call: the COMMITTED delta between two HEAD shas (a
//! `git diff --name-status` subprocess — fine per ADR-4's precedent that
//! blame/diff-shaped work shells out rather than reimplementing git's own
//! algorithms) and the UNCOMMITTED dirty check over whatever working-tree
//! paths the gate held while suspended (the racy-git stat-then-hash
//! protocol: trust a stat that differs from the last record, hash to be
//! sure when it doesn't — see `dirty_check`).
//!
//! # Why `run_git_diff` has no error enum at all
//!
//! This crate's other git-subprocess wrappers (`diff::DiffError`,
//! `checkout::CheckoutError`, `blame::BlameError`, `sessiondiff::
//! git_diff::DiffError` — see `diff.rs`'s module doc, "Why `DiffError`
//! isn't shared with its siblings", for why each of THOSE is its own
//! enum rather than one shared type) all surface a spawn/exit failure to an
//! HTTP caller who can retry or fix their input. [`run_git_diff`] has no
//! caller like that at all — it runs INSIDE the live-mirror watcher's own
//! background reconcile loop (`mirror::watcher`), off the request path
//! entirely, so a `git diff` failure here (a corrupt worktree, git itself
//! missing, ...) has no HTTP response to attach to. It deliberately
//! degrades instead: `tracing::warn!` plus an empty `(changed, removed)` —
//! the SAME "log and continue" posture `dirty_check` below takes for a
//! `fs::metadata` failure (treated as `removed`, not propagated). Giving
//! this fn a sixth error enum just to immediately discard every variant at
//! its one call site would add a type with no reader.

use crate::git::{EntryKind, GitRepo};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

/// Per-repo cache the dirty check consults across reconciles — keyed by
/// repo-relative path. Not persisted across a daemon restart (a restart's
/// startup reconcile already re-derives everything from the committed tree
/// plus a fresh dirty pass, so an empty cache on boot is correct, just
/// slightly more conservative for the very first reconcile after a
/// restart).
#[derive(Debug, Default)]
pub struct DirtyCache {
    entries: HashMap<PathBuf, FileStamp>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct FileStamp {
    mtime: Option<SystemTime>,
    len: u64,
    hash: Option<blake3::Hash>,
}

impl DirtyCache {
    pub fn new() -> Self {
        Self::default()
    }
}

/// The committed side of a reconcile: everything that changed between two
/// resolved HEADs, as git itself sees it — status-aware (`A`/`M`/`T`
/// bucket into `changed`, `D` buckets into `removed`).
///
/// `old = None` is the startup shape ("HEAD tree vs nothing = everything,
/// but expressed as the same call" — the implementation plan's own
/// wording): every blob path in `new`'s tree is reported as `changed`,
/// nothing `removed`. `old == Some(new)` (HEAD didn't actually move, e.g.
/// an aborted merge that returns to the exact starting commit) short-
/// circuits to empty — the dirty check is still run by the caller
/// regardless, since a held working-tree path can be dirty independent of
/// whether HEAD moved at all.
pub fn committed_delta(
    repo: &GitRepo,
    root: &Path,
    old: Option<gix::ObjectId>,
    new: gix::ObjectId,
) -> (Vec<PathBuf>, Vec<PathBuf>) {
    match old {
        None => (recursive_tree_files(repo, &new.to_string()), Vec::new()),
        Some(old_id) if old_id == new => (Vec::new(), Vec::new()),
        Some(old_id) => run_git_diff(root, &old_id.to_string(), &new.to_string()),
    }
}

/// Every blob (file/symlink) path under `rev`'s tree, recursively. A
/// submodule entry is reported as its OWN leaf path and never descended
/// into (mirrors W1.3's `EntryKind::Submodule` "pinned sha, no
/// traversal" rule) — this is what satisfies "reconcile reports the
/// submodule path (as a path — no descent)" for both the startup-listing
/// shape here AND the `git diff`-based shape in `run_git_diff` (git's own
/// diff output already treats a gitlink change as one path line).
fn recursive_tree_files(repo: &GitRepo, rev: &str) -> Vec<PathBuf> {
    let mut out = Vec::new();
    walk_tree(repo, rev, "", &mut out);
    out
}

fn walk_tree(repo: &GitRepo, rev: &str, prefix: &str, out: &mut Vec<PathBuf>) {
    let Ok(entries) = repo.list_tree(rev, prefix) else {
        return;
    };
    for entry in entries {
        let rel = if prefix.is_empty() {
            entry.name.clone()
        } else {
            format!("{prefix}/{}", entry.name)
        };
        match entry.kind {
            EntryKind::Dir => walk_tree(repo, rev, &rel, out),
            EntryKind::File | EntryKind::Symlink | EntryKind::Submodule => {
                out.push(PathBuf::from(rel));
            }
        }
    }
}

/// `git diff --name-status --no-renames old..new` in `root`, parsed into
/// (changed, removed). `--no-renames` keeps the status alphabet to
/// `A`/`M`/`T`/`D` (no `R`/`C` pairs to reassemble) — matches ADR-3's own
/// wording (`git diff --name-only old..new`); `--name-status` (not
/// `--name-only`) is the one addition, needed to separate `removed` from
/// `changed` since the sink's contract keeps them as two lists.
fn run_git_diff(root: &Path, old: &str, new: &str) -> (Vec<PathBuf>, Vec<PathBuf>) {
    let output = std::process::Command::new("git")
        .arg("-C")
        .arg(root)
        .args(["diff", "--name-status", "--no-renames", old, new])
        .output();
    let out = match output {
        Ok(o) => o,
        Err(e) => {
            tracing::warn!(root = %root.display(), old, new, error = %e, "reconcile: git diff spawn failed");
            return (Vec::new(), Vec::new());
        }
    };
    if !out.status.success() {
        tracing::warn!(
            root = %root.display(), old, new,
            stderr = %String::from_utf8_lossy(&out.stderr),
            "reconcile: git diff --name-status failed",
        );
        return (Vec::new(), Vec::new());
    }
    let mut changed = Vec::new();
    let mut removed = Vec::new();
    for line in String::from_utf8_lossy(&out.stdout).lines() {
        let mut parts = line.splitn(2, '\t');
        let (Some(status), Some(path)) = (parts.next(), parts.next()) else {
            continue;
        };
        match status.chars().next() {
            Some('D') => removed.push(PathBuf::from(path)),
            Some(_) => changed.push(PathBuf::from(path)),
            None => {}
        }
    }
    (changed, removed)
}

/// The uncommitted side of a reconcile: the racy-git stat-then-hash
/// protocol over `held` (absolute paths, as delivered by `notify`,
/// relative to `root`). Paths already present in `already_changed`/
/// `already_removed` (the committed side) are skipped — a file that's part
/// of the just-applied commit shouldn't ALSO show up as an independent
/// "uncommitted dirty" entry.
///
/// Protocol: if the current `(mtime, len)` differs from the cache's last
/// record for this path, trust the stat — it's dirty, no hash needed
/// (cheap path, the common case). If the stat is UNCHANGED since the last
/// record, that's exactly racy-git's blind spot (a same-tick edit can
/// leave an identical mtime at second/sub-second granularity), so hash the
/// content and compare against the cached hash before deciding.
pub fn dirty_check(
    root: &Path,
    held: &[PathBuf],
    cache: &mut DirtyCache,
    already_changed: &[PathBuf],
    already_removed: &[PathBuf],
) -> (Vec<PathBuf>, Vec<PathBuf>) {
    let mut changed = Vec::new();
    let mut removed = Vec::new();
    for abs in held {
        let Ok(rel) = abs.strip_prefix(root) else {
            continue;
        };
        let rel = rel.to_path_buf();
        if already_changed.contains(&rel) || already_removed.contains(&rel) {
            continue;
        }
        match std::fs::metadata(abs) {
            Ok(meta) => {
                let mtime = meta.modified().ok();
                let len = meta.len();
                let is_dirty = match cache.entries.get(&rel) {
                    Some(prev) if prev.mtime == mtime && prev.len == len => {
                        // Racy-git: stat alone can't be trusted here.
                        content_hash(abs) != prev.hash
                    }
                    _ => true,
                };
                let hash = content_hash(abs);
                cache
                    .entries
                    .insert(rel.clone(), FileStamp { mtime, len, hash });
                if is_dirty {
                    changed.push(rel);
                }
            }
            Err(_) => {
                cache.entries.remove(&rel);
                removed.push(rel);
            }
        }
    }
    (changed, removed)
}

fn content_hash(path: &Path) -> Option<blake3::Hash> {
    std::fs::read(path).ok().map(|bytes| blake3::hash(&bytes))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn git(dir: &Path, args: &[&str]) {
        let out = std::process::Command::new("git")
            .arg("-C")
            .arg(dir)
            .args(args)
            .output()
            .expect("git runs");
        assert!(
            out.status.success(),
            "git {args:?} failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }

    fn init_repo(dir: &Path) {
        git(dir, &["init", "-q", "-b", "main"]);
        git(dir, &["config", "user.email", "t@example.com"]);
        git(dir, &["config", "user.name", "T"]);
    }

    #[test]
    fn run_git_diff_separates_changed_and_removed() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path();
        init_repo(dir);
        std::fs::write(dir.join("a.txt"), "a1").unwrap();
        std::fs::write(dir.join("b.txt"), "b1").unwrap();
        git(dir, &["add", "-A"]);
        git(dir, &["commit", "-q", "-m", "c1"]);
        let old = std::process::Command::new("git")
            .arg("-C")
            .arg(dir)
            .args(["rev-parse", "HEAD"])
            .output()
            .unwrap();
        let old_sha = String::from_utf8(old.stdout).unwrap().trim().to_string();

        std::fs::write(dir.join("a.txt"), "a2").unwrap();
        std::fs::remove_file(dir.join("b.txt")).unwrap();
        std::fs::write(dir.join("c.txt"), "c1").unwrap();
        git(dir, &["add", "-A"]);
        git(dir, &["commit", "-q", "-m", "c2"]);
        let new = std::process::Command::new("git")
            .arg("-C")
            .arg(dir)
            .args(["rev-parse", "HEAD"])
            .output()
            .unwrap();
        let new_sha = String::from_utf8(new.stdout).unwrap().trim().to_string();

        let (changed, removed) = run_git_diff(dir, &old_sha, &new_sha);
        assert_eq!(
            changed,
            vec![PathBuf::from("a.txt"), PathBuf::from("c.txt")]
        );
        assert_eq!(removed, vec![PathBuf::from("b.txt")]);
    }

    #[test]
    fn dirty_check_trusts_a_changed_stat_without_hashing_first_time() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path();
        std::fs::write(dir.join("dirty.txt"), "v1").unwrap();
        let mut cache = DirtyCache::new();
        let held = vec![dir.join("dirty.txt")];
        let (changed, removed) = dirty_check(dir, &held, &mut cache, &[], &[]);
        assert_eq!(changed, vec![PathBuf::from("dirty.txt")]);
        assert!(removed.is_empty());
    }

    #[test]
    fn dirty_check_reports_removed_for_a_gone_path() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path();
        let held = vec![dir.join("gone.txt")];
        let mut cache = DirtyCache::new();
        let (changed, removed) = dirty_check(dir, &held, &mut cache, &[], &[]);
        assert!(changed.is_empty());
        assert_eq!(removed, vec![PathBuf::from("gone.txt")]);
    }

    #[test]
    fn dirty_check_skips_paths_already_covered_by_the_committed_diff() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path();
        std::fs::write(dir.join("a.txt"), "v1").unwrap();
        let mut cache = DirtyCache::new();
        let held = vec![dir.join("a.txt")];
        let already_changed = vec![PathBuf::from("a.txt")];
        let (changed, removed) = dirty_check(dir, &held, &mut cache, &already_changed, &[]);
        assert!(changed.is_empty(), "already covered by the commit diff");
        assert!(removed.is_empty());
    }

    #[test]
    fn dirty_check_same_stat_same_content_is_not_dirty() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path();
        let path = dir.join("stable.txt");
        std::fs::write(&path, "same").unwrap();
        let mut cache = DirtyCache::new();
        let held = vec![path.clone()];

        // First pass seeds the cache (reports dirty — no prior record).
        let (changed1, _) = dirty_check(dir, &held, &mut cache, &[], &[]);
        assert_eq!(changed1, vec![PathBuf::from("stable.txt")]);

        // Second pass: nothing touched the file at all — must not report.
        let (changed2, removed2) = dirty_check(dir, &held, &mut cache, &[], &[]);
        assert!(
            changed2.is_empty(),
            "an unchanged file must not re-report as dirty"
        );
        assert!(removed2.is_empty());
    }

    #[test]
    fn committed_delta_none_lists_whole_tree_nothing_removed() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path();
        init_repo(dir);
        std::fs::write(dir.join("a.txt"), "a").unwrap();
        std::fs::create_dir_all(dir.join("sub")).unwrap();
        std::fs::write(dir.join("sub/b.txt"), "b").unwrap();
        git(dir, &["add", "-A"]);
        git(dir, &["commit", "-q", "-m", "c1"]);

        let repo = GitRepo::open(dir).unwrap();
        let head = repo.resolve("HEAD").unwrap();
        let (changed, removed) = committed_delta(&repo, dir, None, head);
        let mut changed = changed;
        changed.sort();
        assert_eq!(
            changed,
            vec![PathBuf::from("a.txt"), PathBuf::from("sub/b.txt")]
        );
        assert!(removed.is_empty());
    }

    #[test]
    fn committed_delta_same_head_is_empty() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path();
        init_repo(dir);
        std::fs::write(dir.join("a.txt"), "a").unwrap();
        git(dir, &["add", "-A"]);
        git(dir, &["commit", "-q", "-m", "c1"]);
        let repo = GitRepo::open(dir).unwrap();
        let head = repo.resolve("HEAD").unwrap();
        let (changed, removed) = committed_delta(&repo, dir, Some(head), head);
        assert!(changed.is_empty());
        assert!(removed.is_empty());
    }
}
