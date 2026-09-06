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
/// RECURSIVELY, skipping [`ALWAYS_SKIP_DIRS`], any bare top-level name
/// found in `.gitignore` (see [`gitignore_top_level_dirs`]), and any
/// TOP-LEVEL submodule directory (see [`gitmodules_submodule_paths`] — a
/// checked-out submodule's tree is not this repo's content; descending
/// into it only burns watch descriptors on churn
/// [`event_skip_patterns`] would drop anyway).
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
    skip.extend(
        gitmodules_submodule_paths(root)
            .into_iter()
            .filter(|p| !p.contains('/')),
    );

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

/// One directory-shaped `.gitignore` line, normalised (no leading/trailing
/// `/`). Two shapes, following git's own anchoring rule: a pattern with NO
/// slash anywhere matches a directory of that name at ANY depth; a pattern
/// with a leading or embedded slash is anchored at the repo root.
#[derive(Debug, Clone, PartialEq, Eq)]
enum DirPattern {
    /// `vendor`, `node_modules/` — any-depth directory name.
    Bare(String),
    /// `/dist`, `.claude/worktrees/`, `apps/server/log/` — root-relative.
    Anchored(String),
}

/// Lightweight `.gitignore` scan, directory-shaped lines only: a line that
/// names a directory (`target/`, `.claude/worktrees/`, `/dist`, or a bare
/// `node_modules`) with no wildcard characters contributes one
/// [`DirPattern`]. Comments, negations and wildcard patterns are skipped.
///
/// Deliberately NOT a real gitignore engine — mirrors `kb_core::watcher`'s
/// own `path_matches_skip_pattern` precedent ("if you need a real
/// gitignore engine, add the `ignore` crate"). A pattern this heuristic
/// misses just costs a few extra watched inodes or a few extra events,
/// never a correctness bug (reconcile only ever reports TRACKED paths,
/// which gitignore by definition does not cover).
fn parse_gitignore_dir_patterns(raw: &str) -> Vec<DirPattern> {
    let mut out = Vec::new();
    for line in raw.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') || line.starts_with('!') {
            continue;
        }
        let anchored = line.starts_with('/');
        let stripped = line.strip_prefix('/').unwrap_or(line);
        let stripped = stripped.strip_suffix('/').unwrap_or(stripped);
        if stripped.is_empty()
            || stripped.contains('*')
            || stripped.contains('?')
            || stripped.contains('[')
        {
            continue;
        }
        if anchored || stripped.contains('/') {
            out.push(DirPattern::Anchored(stripped.to_string()));
        } else {
            out.push(DirPattern::Bare(stripped.to_string()));
        }
    }
    out
}

/// Lightweight `.gitignore`-top-level-only scan: a line shaped exactly like
/// a bare directory name (`target/`, `node_modules`, an optional leading
/// `/` and/or trailing `/`, no wildcards, no embedded `/`) contributes its
/// own name to the watch-registration skip-set.
///
/// This only prunes WATCH REGISTRATION (an inotify-budget concern); it
/// never decides ingest eligibility — see [`event_skip_patterns`] for the
/// event-routing half, which unlike registration CAN honour multi-segment
/// patterns.
pub fn gitignore_top_level_dirs(root: &Path) -> HashSet<String> {
    let Ok(raw) = std::fs::read_to_string(root.join(".gitignore")) else {
        return HashSet::new();
    };
    parse_gitignore_dir_patterns(&raw)
        .into_iter()
        .filter_map(|p| match p {
            // A root-anchored single-segment pattern (`/dist`) is, for
            // top-level registration, the same thing as a bare name.
            DirPattern::Bare(name) => Some(name),
            DirPattern::Anchored(path) if !path.contains('/') => Some(path),
            DirPattern::Anchored(_) => None,
        })
        .collect()
}

/// Skip patterns for the mirror's WORKING-TREE EVENT filter
/// (`process_flush`), expressed in `kb_core::watcher`'s skip-pattern
/// grammar (see `path_matches_skip_pattern`'s doc). Four sources:
///
/// - [`ALWAYS_SKIP_DIRS`], as any-depth `**/name/**` patterns — the
///   registration skip is top-level-only, but a NESTED `target/` or
///   `node_modules/` (a crate subdir, a JS sub-app) is just as much
///   build output.
/// - every directory-shaped line of the repo's root `.gitignore`
///   ([`parse_gitignore_dir_patterns`]): bare names become any-depth
///   `**/name/**`, anchored patterns become `path/**` prefixes. This is
///   what covers the patterns registration structurally cannot —
///   multi-segment ones like `.claude/worktrees/` or `apps/server/log/`
///   (agent-session worktrees and Rails log/tmp churn INSIDE the watched
///   tree: untracked by git, so the HEAD-tree walk and every reconcile
///   never report them, but a recursive inotify watch observes every
///   write; measured on a busy multi-worktree host: 728k junk `files` rows, ~96% of the store,
///   from months of `.claude/worktrees/agent-*` indexing).
/// - SELF-IGNORING top-level directories: a child dir whose own
///   `.gitignore` is exactly `*` (the agent-worktree convention, e.g.
///   `.grokclaude-worktrees/.gitignore`) — the root `.gitignore` never
///   mentions these, but the dir's own ignore file declares every
///   content un-trackable, so its events are never ingest-worthy either.
/// - every SUBMODULE path from the repo's `.gitmodules`
///   ([`parse_gitmodules_paths`]), as a root-anchored `path/**` prefix.
///   A submodule's working tree is not this repo's content: the HEAD-tree
///   walk and `git diff` report the gitlink as one leaf path (the sink
///   already documents "a submodule gitlink has nothing to read"), but a
///   recursive inotify watch descends INTO the checked-out submodule and
///   reports its churn as ordinary working-tree events — with no
///   `.gitignore` line covering it, nothing else in this filter stops
///   them (measured on h4o: ~21k junk `files` rows under `legacy/`).
///
/// Negations and wildcard patterns stay out of scope (skipped by the
/// parser): the worst case of a missed pattern is extra events, and the
/// reconcile backstop is unaffected either way since it derives from
/// tracked paths only.
pub fn event_skip_patterns(root: &Path) -> Vec<String> {
    let mut out: Vec<String> = ALWAYS_SKIP_DIRS
        .iter()
        .map(|name| format!("**/{name}/**"))
        .collect();
    if let Ok(raw) = std::fs::read_to_string(root.join(".gitignore")) {
        for pat in parse_gitignore_dir_patterns(&raw) {
            match pat {
                DirPattern::Bare(name) => out.push(format!("**/{name}/**")),
                DirPattern::Anchored(path) => out.push(format!("{path}/**")),
            }
        }
    }
    for name in self_ignoring_top_level_dirs(root) {
        out.push(format!("{name}/**"));
    }
    for path in gitmodules_submodule_paths(root) {
        out.push(format!("{path}/**"));
    }
    out
}

/// Submodule paths declared by one `.gitmodules` file — a conservative
/// git-config-shaped scan for `path = <p>` lines, nothing more. Values are
/// trimmed and optionally unquoted; anything with a wildcard character, an
/// absolute/`..` path, or a missing/empty value is skipped (git itself
/// requires submodule paths to be relative, so a line this parser rejects
/// was never a usable skip target anyway). Same deliberate-not-a-real-
/// parser posture as [`parse_gitignore_dir_patterns`].
fn parse_gitmodules_paths(raw: &str) -> Vec<String> {
    let mut out = Vec::new();
    for line in raw.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') || line.starts_with(';') {
            continue;
        }
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        if key.trim() != "path" {
            continue;
        }
        let value = value.trim();
        let value = value
            .strip_prefix('"')
            .and_then(|v| v.strip_suffix('"'))
            .unwrap_or(value)
            .trim();
        if value.is_empty()
            || value.starts_with('/')
            || value.contains("..")
            || value.contains('*')
            || value.contains('?')
            || value.contains('[')
        {
            continue;
        }
        out.push(value.trim_end_matches('/').to_string());
    }
    out
}

/// Every submodule path declared in the repo's root `.gitmodules`
/// (root-relative, as git writes them). A missing or unreadable
/// `.gitmodules` — the common case, most repos have none — is an empty
/// list, not an error.
pub fn gitmodules_submodule_paths(root: &Path) -> Vec<String> {
    let Ok(raw) = std::fs::read_to_string(root.join(".gitmodules")) else {
        return Vec::new();
    };
    parse_gitmodules_paths(&raw)
}

/// Immediate child directories of `root` whose own `.gitignore` is exactly
/// `*` — the "everything in here is scratch" convention used by agent
/// session dirs (`.grokclaude-worktrees/`, changelog scratch dirs, …).
/// Such a dir's contents are un-trackable by git's own rules regardless of
/// what the root `.gitignore` says.
fn self_ignoring_top_level_dirs(root: &Path) -> Vec<String> {
    let mut out = Vec::new();
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
        let Ok(raw) = std::fs::read_to_string(entry.path().join(".gitignore")) else {
            continue;
        };
        if raw.trim() == "*" {
            if let Some(name) = entry.file_name().to_str() {
                out.push(name.to_string());
            }
        }
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
    fn parse_gitignore_dir_patterns_keeps_multi_segment_and_marks_anchoring() {
        let pats = parse_gitignore_dir_patterns(
            "# comment\n\nbuild/\n/dist\nvendor\n*.log\nnested/dir/\n!keep\n.claude/worktrees/\n",
        );
        assert_eq!(
            pats,
            vec![
                DirPattern::Bare("build".to_string()),
                DirPattern::Anchored("dist".to_string()),
                DirPattern::Bare("vendor".to_string()),
                // `*.log` (wildcard) and `!keep` (negation) are out of grammar.
                DirPattern::Anchored("nested/dir".to_string()),
                DirPattern::Anchored(".claude/worktrees".to_string()),
            ]
        );
    }

    #[test]
    fn event_skip_patterns_cover_agent_worktrees_and_nested_churn() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        // The observed shape: multi-segment agent-worktree and Rails log/tmp
        // patterns registration could never express.
        std::fs::write(
            root.join(".gitignore"),
            "apps/server/log/\napps/server/tmp/\n.claude/worktrees/\n.codex-worktrees/\n*.log\n",
        )
        .unwrap();
        // …plus a self-ignoring dir (`.grokclaude-worktrees/.gitignore`
        // containing exactly `*`), which no root `.gitignore` line names.
        let self_ignoring = root.join(".grokclaude-worktrees");
        std::fs::create_dir_all(&self_ignoring).unwrap();
        std::fs::write(self_ignoring.join(".gitignore"), "*\n").unwrap();
        // …a dir whose .gitignore has real content is NOT self-ignoring…
        let ordinary = root.join("docs");
        std::fs::create_dir_all(&ordinary).unwrap();
        std::fs::write(ordinary.join(".gitignore"), "*.tmp\n").unwrap();
        // …and neither is a FILE named like one.
        std::fs::write(root.join("README.md"), "x\n").unwrap();

        let pats = event_skip_patterns(root);
        let matches = |rel: &str| {
            let basename = rel.rsplit('/').next().unwrap_or(rel);
            pats.iter()
                .any(|p| kb_core::watcher::path_matches_skip_pattern(rel, basename, p))
        };

        // The observed churn vectors are all covered.
        assert!(matches(".claude/worktrees/agent-0129abc/src/main.rs"));
        assert!(matches(".codex-worktrees/f6-plan/apps/server/Gemfile"));
        assert!(matches(".grokclaude-worktrees/gc-01KZX/x.rb"));
        assert!(matches(".grokclaude-worktrees")); // the dir itself (a Remove)
        assert!(matches("apps/server/log/production.log"));
        assert!(matches("apps/server/tmp/pids/server.pid"));
        // ALWAYS_SKIP_DIRS as any-depth patterns: a nested build-output dir.
        assert!(matches("crates/sub/target/debug/build.rs"));
        assert!(matches("apps/desktop/node_modules/left-pad/index.js"));

        // Real source paths must NEVER match — including near-misses that
        // share a prefix with an ignored pattern (component-boundary, never
        // substring).
        assert!(!matches("apps/server/app/models/user.rb"));
        assert!(!matches("apps/server/logistics/tracker.rb"));
        assert!(!matches(".claude/workflows/review.md"));
        assert!(!matches("docs/guide.html"));
        assert!(!matches("README.md"));
        // The wildcard `*.log` line is out of grammar: a TRACKED .log file
        // (force-added) still flows — the filter only prunes directories.
        assert!(!matches("docs/CHANGELOG.log"));
    }

    #[test]
    fn event_skip_patterns_survive_a_missing_gitignore() {
        let tmp = tempfile::tempdir().unwrap();
        // No .gitignore at all: only the ALWAYS_SKIP_DIRS patterns remain.
        let pats = event_skip_patterns(tmp.path());
        assert_eq!(
            pats,
            ALWAYS_SKIP_DIRS
                .iter()
                .map(|n| format!("**/{n}/**"))
                .collect::<Vec<_>>()
        );
    }

    #[test]
    fn parse_gitmodules_paths_parses_path_entries() {
        let pats = parse_gitmodules_paths(
            "# a comment\n\
             ; another comment\n\
             [submodule \"legacy\"]\n\
             \tpath = legacy\n\
             \turl = https://example.com/legacy.git\n\
             [submodule \"lib\"]\n\
             \tpath=vendor/lib\n\
             \tpath = \"quoted/one\"\n\
             \tpath = \n\
             \tpath = /absolute\n\
             \tpath = ../escape\n\
             [submodule \"nopath\"]\n\
             \turl = https://example.com/nopath.git\n",
        );
        assert_eq!(
            pats,
            vec![
                "legacy".to_string(),
                "vendor/lib".to_string(),
                "quoted/one".to_string()
            ]
        );
    }

    #[test]
    fn gitmodules_submodule_paths_survive_a_missing_file() {
        let tmp = tempfile::tempdir().unwrap();
        assert_eq!(gitmodules_submodule_paths(tmp.path()), Vec::<String>::new());
    }

    #[test]
    fn event_skip_patterns_cover_submodule_working_trees() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        std::fs::write(
            root.join(".gitmodules"),
            "[submodule \"legacy\"]\n\tpath = legacy\n\turl = https://example.com/legacy.git\n",
        )
        .unwrap();
        let pats = event_skip_patterns(root);
        let matches = |rel: &str| {
            let basename = rel.rsplit('/').next().unwrap_or(rel);
            pats.iter()
                .any(|p| kb_core::watcher::path_matches_skip_pattern(rel, basename, p))
        };
        assert!(matches("legacy/hotel/old.rb"));
        assert!(matches("legacy")); // the gitlink dir itself (a Remove)
        // Component-boundary, never substring: a real sibling dir whose
        // name merely shares the prefix stays watched.
        assert!(!matches("legacy-fixes/new.rb"));
        assert!(!matches("apps/server/app/models/user.rb"));
    }

    #[test]
    fn working_tree_watch_set_skips_top_level_submodule_dirs() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        std::fs::write(
            root.join(".gitmodules"),
            "[submodule \"legacy\"]\n\tpath = legacy\n[submodule \"nested\"]\n\tpath = vendor/nested\n",
        )
        .unwrap();
        std::fs::create_dir_all(root.join("legacy")).unwrap();
        std::fs::create_dir_all(root.join("src")).unwrap();
        let entries = working_tree_watch_set(root);
        let paths: HashSet<_> = entries.iter().map(|e| e.path.clone()).collect();
        assert!(!paths.contains(&root.join("legacy")));
        assert!(paths.contains(&root.join("src")));
        // A NESTED submodule path can't be pruned at registration (only
        // top-level dirs are registered individually) — the event filter
        // covers it; registration just must not break because of it.
        assert!(paths.contains(root));
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
