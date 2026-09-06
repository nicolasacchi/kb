//! Sub-step (a) — which paths the watcher registers, and with which
//! `notify` recursion mode. Two domains per repo: the working tree
//! ([`working_tree_watch_set`]) and the git internals
//! ([`git_dir_watch_set`]) — worktree-correct per ADR-3's review hardening
//! (a linked worktree's private state lives under its OWN `git_dir`; shared
//! refs live under `common_dir`, which W1.3's `GitRepo` already resolves).

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

/// One filesystem path this watcher should register, plus the `notify`
/// recursive mode to register it with.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WatchEntry {
    pub path: PathBuf,
    pub recursive: bool,
}

/// Always-skip directory names under a repo's working tree — cheap,
/// independent of `.gitignore` contents. `.git` is skipped because its
/// contents are covered by the SEPARATE git-dir watch domain (registering
/// it again here would double inotify's per-directory cost for the same
/// events, on top of counting objects/ — potentially thousands of loose
/// object files — against the watch budget for no benefit); `target`/
/// `node_modules` are the two build-output directories big enough, in this
/// fleet's own repos, to matter on their own (Rust + JS/TS monorepos both
/// live here).
pub const ALWAYS_SKIP_DIRS: &[&str] = &[".git", "target", "node_modules"];

/// Sub-step (a) — working-tree watch-set for one repo root.
///
/// `notify` has no "recursive except this subdir" primitive, so full
/// working-tree coverage minus `.git`/build-output dirs is built by hand:
/// the root itself is watched NON-recursively (root-level file
/// creates/edits, e.g. a `Cargo.toml` edit, plus noticing a brand-new
/// top-level entry exists) and every immediate child DIRECTORY is watched
/// RECURSIVELY, skipping [`ALWAYS_SKIP_DIRS`] and any bare top-level name
/// found in `.gitignore` (see [`gitignore_top_level_dirs`]).
///
/// Known limitation: a new top-level directory created after boot is
/// covered by the non-recursive root watch (its own creation is seen) but
/// its CONTENTS are not automatically watched recursively until the daemon
/// restarts and re-derives the watch-set — mirrors kb's own "reconcile is
/// the backstop, not every corner of live-watch" model (`kb-core`'s own
/// watcher accepts the analogous gap for the mtime-map fast path) rather
/// than adding dynamic re-arm machinery for what is, for this fleet, a rare
/// event (5-15 repos, not thousands of directories appearing at runtime).
pub fn working_tree_watch_set(root: &Path) -> Vec<WatchEntry> {
    let mut skip: HashSet<String> = ALWAYS_SKIP_DIRS.iter().map(|s| s.to_string()).collect();
    skip.extend(gitignore_top_level_dirs(root));

    let mut out = vec![WatchEntry {
        path: root.to_path_buf(),
        recursive: false,
    }];
    let Ok(entries) = std::fs::read_dir(root) else {
        return out;
    };
    for entry in entries.flatten() {
        let Ok(file_type) = entry.file_type() else {
            continue;
        };
        if !file_type.is_dir() {
            continue;
        }
        let name = entry.file_name();
        let Some(name_str) = name.to_str() else {
            continue;
        };
        if skip.contains(name_str) {
            continue;
        }
        out.push(WatchEntry {
            path: entry.path(),
            recursive: true,
        });
    }
    out
}

/// Lightweight `.gitignore`-top-level-only scan: a line shaped exactly like
/// a bare directory name (`target/`, `node_modules`, an optional leading
/// `/` and/or trailing `/`, no wildcards, no embedded `/`) contributes its
/// own name to the watch-registration skip-set.
///
/// Deliberately NOT a real gitignore engine — mirrors `kb_core::watcher`'s
/// own `path_matches_skip_pattern` precedent ("if you need a real
/// gitignore engine, add the `ignore` crate"). This only prunes WATCH
/// REGISTRATION (an inotify-budget concern); it never decides ingest
/// eligibility, so a pattern this heuristic misses just costs a few extra
/// watched inodes, never a correctness bug.
pub fn gitignore_top_level_dirs(root: &Path) -> HashSet<String> {
    let mut out = HashSet::new();
    let Ok(raw) = std::fs::read_to_string(root.join(".gitignore")) else {
        return out;
    };
    for line in raw.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') || line.starts_with('!') {
            continue;
        }
        let stripped = line.strip_prefix('/').unwrap_or(line);
        let stripped = stripped.strip_suffix('/').unwrap_or(stripped);
        if stripped.is_empty()
            || stripped.contains('/')
            || stripped.contains('*')
            || stripped.contains('?')
            || stripped.contains('[')
        {
            continue;
        }
        out.insert(stripped.to_string());
    }
    out
}

/// Sub-step (a) — git-internals watch-set for one repo: this repo's own
/// private `git_dir` (worktree-correct: a linked worktree's `HEAD`/
/// `index`/state dirs live in `<main>/.git/worktrees/<name>`, a DIFFERENT
/// path from the main worktree's, so registering per-`RepoRef` — not
/// per-physical-repo — is what keeps the two isolated) plus the shared
/// `common_dir` for `packed-refs` and `refs/`. Every entry is
/// NON-recursive except `common_dir/refs` (branches can nest,
/// `refs/heads/feature/x` needs recursion to see the leaf ref file).
pub fn git_dir_watch_set(git_dir: &Path, common_dir: &Path) -> Vec<WatchEntry> {
    vec![
        // HEAD, index, index.lock, MERGE_HEAD, CHERRY_PICK_HEAD,
        // BISECT_LOG, and the rebase-merge/rebase-apply dirs (as
        // directory-entry create/remove — never descended into) are all
        // direct children of `git_dir`.
        WatchEntry {
            path: git_dir.to_path_buf(),
            recursive: false,
        },
        // `logs/HEAD` — the reflog; every commit/amend/reset/rebase-step
        // touches it, a second independent HEAD-moved signal alongside the
        // HEAD file itself. Doesn't exist yet on a freshly `git init`'d
        // repo (no ref update has happened) — the arm step tolerates that.
        WatchEntry {
            path: git_dir.join("logs"),
            recursive: false,
        },
        // `packed-refs` lives directly in `common_dir`.
        WatchEntry {
            path: common_dir.to_path_buf(),
            recursive: false,
        },
        WatchEntry {
            path: common_dir.join("refs"),
            recursive: true,
        },
    ]
}

/// Dedup by canonical registration path — the linked-worktree case
/// registers `common_dir` (as both `linked`'s `common_dir` entry and, for
/// the main worktree, `main`'s `git_dir` entry) potentially twice; a
/// `recursive: true` entry wins over a `recursive: false` one for the same
/// path so no domain's coverage is silently narrowed by dedup order.
pub fn dedup(entries: Vec<WatchEntry>) -> Vec<WatchEntry> {
    let mut map: HashMap<PathBuf, bool> = HashMap::new();
    for e in entries {
        map.entry(e.path)
            .and_modify(|r| *r = *r || e.recursive)
            .or_insert(e.recursive);
    }
    map.into_iter()
        .map(|(path, recursive)| WatchEntry { path, recursive })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn working_tree_set_skips_git_target_node_modules() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        for d in [".git", "target", "node_modules", "src", "docs"] {
            std::fs::create_dir_all(root.join(d)).unwrap();
        }
        let entries = working_tree_watch_set(root);
        let paths: HashSet<_> = entries.iter().map(|e| e.path.clone()).collect();
        assert!(paths.contains(root));
        assert!(paths.contains(&root.join("src")));
        assert!(paths.contains(&root.join("docs")));
        assert!(!paths.contains(&root.join(".git")));
        assert!(!paths.contains(&root.join("target")));
        assert!(!paths.contains(&root.join("node_modules")));

        // Root itself is non-recursive; children are recursive.
        let root_entry = entries.iter().find(|e| e.path == root).unwrap();
        assert!(!root_entry.recursive);
        let src_entry = entries.iter().find(|e| e.path == root.join("src")).unwrap();
        assert!(src_entry.recursive);
    }

    #[test]
    fn gitignore_top_level_dirs_parses_bare_names_only() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(
            tmp.path().join(".gitignore"),
            "# comment\n\nbuild/\n/dist\nvendor\n*.log\nnested/dir\n!keep\n",
        )
        .unwrap();
        let got = gitignore_top_level_dirs(tmp.path());
        assert_eq!(
            got,
            HashSet::from([
                "build".to_string(),
                "dist".to_string(),
                "vendor".to_string()
            ])
        );
    }

    #[test]
    fn gitignore_top_level_dirs_prunes_watch_set() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        std::fs::write(root.join(".gitignore"), "vendor/\n").unwrap();
        std::fs::create_dir_all(root.join("vendor")).unwrap();
        std::fs::create_dir_all(root.join("src")).unwrap();
        let entries = working_tree_watch_set(root);
        let paths: HashSet<_> = entries.iter().map(|e| e.path.clone()).collect();
        assert!(!paths.contains(&root.join("vendor")));
        assert!(paths.contains(&root.join("src")));
    }

    #[test]
    fn git_dir_watch_set_covers_expected_paths() {
        let git_dir = PathBuf::from("/repo/.git");
        let common_dir = git_dir.clone();
        let entries = git_dir_watch_set(&git_dir, &common_dir);
        let paths: HashSet<_> = entries.iter().map(|e| e.path.clone()).collect();
        assert!(paths.contains(&git_dir));
        assert!(paths.contains(&git_dir.join("logs")));
        assert!(paths.contains(&common_dir.join("refs")));
        let refs_entry = entries
            .iter()
            .find(|e| e.path == common_dir.join("refs"))
            .unwrap();
        assert!(refs_entry.recursive, "refs/ must be watched recursively");
    }

    #[test]
    fn dedup_merges_by_path_recursive_wins() {
        let p = PathBuf::from("/a/b");
        let entries = vec![
            WatchEntry {
                path: p.clone(),
                recursive: false,
            },
            WatchEntry {
                path: p.clone(),
                recursive: true,
            },
        ];
        let out = dedup(entries);
        assert_eq!(out.len(), 1);
        assert!(out[0].recursive);
    }
}
