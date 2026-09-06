//! `GET /api/status?repo=` and `GET /api/tree?worktree=1` (V70-A3X) — the
//! two working-tree-facing surfaces the recon's documented ODB/working-tree
//! ASYMMETRY was missing: `GET /api/tree` (no `?worktree=`) is an ODB read
//! at a ref (defaulting to `HEAD`) while `GET /api/file` (no `ref`) reads
//! the live working tree — a caller had no way to LIST the same working
//! tree `/api/file` reads, nor to ask "what's actually dirty right now" at
//! path granularity (`repo_state::is_dirty` exists, but only as a bare
//! boolean feeding `GET /api/repo-state`'s conflict-detection use case).
//! Both are plain `git` subprocess wrappers — this crate's Nth one, own
//! error enum, same convention as `checkout`/`history`/`diff`/
//! `repo_state` before it (see `diff.rs`'s module doc).
//!
//! [`RepoStatus`] (`git status --porcelain=v2 -z`) is cached per
//! [`crate::store::Store::generation`] (the same counter `mirror.updated`
//! bumps rise from — see `store.rs`'s field doc — so a repeat poll between
//! two file-content mutations is served from cache rather than re-shelling
//! to `git` every time). [`list_worktree_dir`] (`git ls-files -z --cached
//! --others --exclude-standard`) is NOT cached — `/api/tree`'s existing
//! ODB-read branch isn't either, and a directory listing is cheap enough
//! per call that adding a second cache axis wasn't worth the complexity.
//!
//! # Porcelain v2 `-z` framing
//!
//! With `-z`, git NUL-terminates every record instead of newline-terminating
//! it, and — ONLY for rename/copy (`2`) records — the `path` and `origPath`
//! are themselves NUL-separated rather than tab-separated. That means a
//! flat split of the whole output on `\0` does NOT yield "one segment per
//! record": a `2` record consumes TWO segments (the header+path, then the
//! bare origPath), everything else consumes exactly one. [`parse_porcelain_v2_z`]
//! walks the segment queue with that in mind rather than zipping it
//! record-by-record.
//!
//! Every OTHER field before the path is single-space-separated, and the
//! path itself is UNQUOTED (unlike porcelain v1 without `-z`, which
//! C-quotes a path containing a space or control byte) — so a path may
//! itself legitimately contain spaces, and naive `split_whitespace()` over
//! a whole record would misparse it. [`nth_space_split`] instead splits
//! only the FIRST `n` single-space boundaries and returns everything after
//! the `n`th as one opaque tail string.

use std::collections::HashMap;
use std::path::Path;
use std::process::Command;
use std::sync::{Arc, Mutex};

use crate::search::GenCached;
use crate::store::Store;

#[derive(Debug, thiserror::Error)]
pub enum StatusError {
    #[error("failed to spawn git: {0}")]
    Spawn(std::io::Error),
    #[error("git failed (exit {status}): {stderr}")]
    GitFailed { status: i32, stderr: String },
}

pub type Result<T> = std::result::Result<T, StatusError>;

/// One path's status — `index`/`worktree` are the porcelain v2 `X`/`Y`
/// status-code characters verbatim (`.` = unmodified in that column; an
/// untracked path reports `?`/`?`, matching git's own `??` short-format
/// convention). `renamed_from` is `Some` ONLY for a rename/copy (`2`-type)
/// record.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct StatusEntry {
    pub path: String,
    pub index: char,
    pub worktree: char,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub renamed_from: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct RepoStatus {
    pub paths: Vec<StatusEntry>,
    pub dirty: bool,
    /// The [`Store::generation`] this snapshot was computed at — lets a
    /// client detect a stale poll without a second round trip.
    pub generation: u64,
}

// --- GET /api/tree?worktree=1 -------------------------------------------

/// One [`list_worktree_dir`] entry — deliberately just `{name, kind}`
/// (unlike `git::tree::TreeEntry`'s `size`/`oid`): an untracked file has no
/// cheap git object id to report (computing one would mean hashing every
/// listed file's bytes, real per-request I/O this route doesn't need to
/// pay), and a size read off `git ls-files` alone isn't available either —
/// a client wanting either already has `GET /api/file` for one specific
/// path. Only `"file"`/`"dir"` kinds — no symlink/submodule distinction
/// (the ODB-read branch's job): this is an ADDITIVE opt-in, not a second
/// full parity implementation of `TreeEntry`.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct WorktreeEntry {
    pub name: String,
    pub kind: &'static str,
}

/// The working-tree listing at `dir` (repo-relative, `""` for the root),
/// ONE LEVEL deep — tracked (`--cached`) + untracked-not-ignored
/// (`--others --exclude-standard`) paths via `git ls-files -z`, which
/// returns the FULL recursive path list under the `dir` pathspec; this fn
/// collapses that down to immediate children, deriving `"dir"` entries from
/// any returned path with a further `/` past `dir` and deduplicating them
/// (many files commonly share one subdirectory). A tracked-but-
/// deleted-from-disk path (`git ls-files --cached` reads the INDEX, which
/// doesn't know the file vanished from disk) is dropped via an explicit
/// `symlink_metadata` existence check — `git ls-files` has no single flag
/// for "cached AND still on disk," so this is a plain Rust-side filter
/// rather than a fourth `ls-files` flag combination.
pub fn list_worktree_dir(repo_root: &Path, dir: &str) -> Result<Vec<WorktreeEntry>> {
    let norm = dir.trim_matches('/');
    let pathspec = if norm.is_empty() { "." } else { norm };
    let out = Command::new("git")
        .arg("-C")
        .arg(repo_root)
        .args([
            "ls-files",
            "-z",
            "--cached",
            "--others",
            "--exclude-standard",
            "--",
        ])
        .arg(pathspec)
        .output()
        .map_err(StatusError::Spawn)?;
    if !out.status.success() {
        return Err(StatusError::GitFailed {
            status: out.status.code().unwrap_or(-1),
            stderr: String::from_utf8_lossy(&out.stderr).trim().to_string(),
        });
    }
    let text = String::from_utf8_lossy(&out.stdout);
    let dir_prefix = if norm.is_empty() {
        String::new()
    } else {
        format!("{norm}/")
    };

    // `BTreeMap` for two reasons at once: stable alphabetical order (no
    // second sort pass) and free dedup of directory names reached by more
    // than one file.
    let mut entries: std::collections::BTreeMap<String, &'static str> =
        std::collections::BTreeMap::new();
    for raw_path in text.split('\0').filter(|s| !s.is_empty()) {
        let Some(rel) = raw_path.strip_prefix(&dir_prefix) else {
            continue; // defensive — the pathspec guarantees this in practice.
        };
        if rel.is_empty() {
            continue;
        }
        // "deleted-in-worktree files omitted" — `--cached` reports index
        // entries regardless of disk presence; `symlink_metadata` (not
        // `metadata`, which follows symlinks and would false-negative on a
        // broken-but-legitimate symlink entry) is the ground truth for "is
        // this still actually there."
        if std::fs::symlink_metadata(repo_root.join(raw_path)).is_err() {
            continue;
        }
        match rel.split_once('/') {
            Some((first, _rest)) => {
                entries.insert(first.to_string(), "dir");
            }
            None => {
                entries.insert(rel.to_string(), "file");
            }
        }
    }
    Ok(entries
        .into_iter()
        .map(|(name, kind)| WorktreeEntry { name, kind })
        .collect())
}

fn run_git_status(repo_root: &Path) -> Result<Vec<u8>> {
    let out = Command::new("git")
        .arg("-C")
        .arg(repo_root)
        .args(["status", "--porcelain=v2", "-z"])
        .output()
        .map_err(StatusError::Spawn)?;
    if !out.status.success() {
        return Err(StatusError::GitFailed {
            status: out.status.code().unwrap_or(-1),
            stderr: String::from_utf8_lossy(&out.stderr).trim().to_string(),
        });
    }
    Ok(out.stdout)
}

/// Split `s` on the first `n` single-space (`' '`) boundaries, returning the
/// `n` leading fields plus everything after the `n`th space verbatim (the
/// "rest" — the path, which may itself contain spaces). `None` if `s` has
/// fewer than `n` space-separated fields (a malformed/unexpected record —
/// callers skip it rather than panic; see the module doc).
fn nth_space_split(s: &str, n: usize) -> Option<(Vec<&str>, &str)> {
    let mut fields = Vec::with_capacity(n);
    let mut start = 0usize;
    for _ in 0..n {
        let rel = s[start..].find(' ')?;
        fields.push(&s[start..start + rel]);
        start += rel + 1;
    }
    Some((fields, &s[start..]))
}

fn xy(fields: &[&str]) -> (char, char) {
    let xy_token = fields.first().copied().unwrap_or("");
    let mut chars = xy_token.chars();
    let index = chars.next().unwrap_or('.');
    let worktree = chars.next().unwrap_or('.');
    (index, worktree)
}

/// Parse `git status --porcelain=v2 -z`'s raw stdout bytes — see the module
/// doc's "Porcelain v2 `-z` framing" section for the record-boundary
/// subtlety. Lossy-decoded (a non-UTF8 path is vanishingly rare and not
/// worth failing the whole parse over — matches `search::text::TextMatch`'s
/// own lossy-decode precedent).
pub fn parse_porcelain_v2_z(output: &[u8]) -> Vec<StatusEntry> {
    let text = String::from_utf8_lossy(output);
    let mut segments: std::collections::VecDeque<&str> =
        text.split('\0').filter(|s| !s.is_empty()).collect();
    let mut entries = Vec::new();

    while let Some(seg) = segments.pop_front() {
        match seg.as_bytes().first() {
            // `1 XY sub mH mI mW hH hI path`
            Some(b'1') => {
                if let Some((fields, path)) = nth_space_split(seg, 8) {
                    let (index, worktree) = xy(&fields[1..]);
                    entries.push(StatusEntry {
                        path: path.to_string(),
                        index,
                        worktree,
                        renamed_from: None,
                    });
                }
            }
            // `2 XY sub mH mI mW hH hI Xscore path` NUL `origPath`
            Some(b'2') => {
                if let Some((fields, path)) = nth_space_split(seg, 9) {
                    let (index, worktree) = xy(&fields[1..]);
                    let orig = segments.pop_front().unwrap_or_default().to_string();
                    entries.push(StatusEntry {
                        path: path.to_string(),
                        index,
                        worktree,
                        renamed_from: Some(orig),
                    });
                }
            }
            // `u XY sub m1 m2 m3 mW h1 h2 h3 path` (unmerged/conflicted)
            Some(b'u') => {
                if let Some((fields, path)) = nth_space_split(seg, 10) {
                    let (index, worktree) = xy(&fields[1..]);
                    entries.push(StatusEntry {
                        path: path.to_string(),
                        index,
                        worktree,
                        renamed_from: None,
                    });
                }
            }
            // `? path` (untracked) — reported as `??`, matching git's own
            // short-format convention for an untracked path.
            Some(b'?') => {
                if let Some((_, path)) = nth_space_split(seg, 1) {
                    entries.push(StatusEntry {
                        path: path.to_string(),
                        index: '?',
                        worktree: '?',
                        renamed_from: None,
                    });
                }
            }
            // `! path` (ignored) — never emitted: this module never passes
            // `--ignored` to `git status`, so this arm is defensive only.
            _ => {}
        }
    }
    entries
}

/// Per-daemon, per-repo [`RepoStatus`] cache, gated on [`Store::generation`]
/// — same "lazy rebuild on a generation counter" shape as `search::files::
/// FileIndex`/`search::symbols::SymbolIndex` (see that module's doc for the
/// rationale). One instance lives in `AppState`.
#[derive(Default)]
pub struct StatusIndex {
    cache: Mutex<HashMap<i64, GenCached<RepoStatus>>>,
}

impl StatusIndex {
    pub fn new() -> Self {
        Self::default()
    }

    /// `repo_root` is a blocking subprocess call — callers must already be
    /// on the blocking pool (see `store.rs`'s 2026-08-31 incident note;
    /// `routes::status_route` wraps this in `state.store.run_blocking`
    /// alongside the `Store::generation()` read so the two can never
    /// observe different generations).
    pub fn status(&self, store: &Store, repo_id: i64, repo_root: &Path) -> Result<Arc<RepoStatus>> {
        let current_gen = store.generation();
        {
            let cache = self.cache.lock().unwrap_or_else(|e| e.into_inner());
            if let Some(entry) = cache.get(&repo_id) {
                if entry.generation == current_gen {
                    return Ok(entry.value.clone());
                }
            }
        }
        let raw = run_git_status(repo_root)?;
        let paths = parse_porcelain_v2_z(&raw);
        let status = Arc::new(RepoStatus {
            dirty: !paths.is_empty(),
            paths,
            generation: current_gen,
        });
        let mut cache = self.cache.lock().unwrap_or_else(|e| e.into_inner());
        cache.insert(
            repo_id,
            GenCached {
                generation: current_gen,
                value: status.clone(),
            },
        );
        Ok(status)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn git(dir: &Path, args: &[&str]) {
        let out = Command::new("git")
            .arg("-C")
            .arg(dir)
            .args(args)
            .output()
            .expect("git runs");
        assert!(
            out.status.success(),
            "git -C {} {:?} failed: {}",
            dir.display(),
            args,
            String::from_utf8_lossy(&out.stderr)
        );
    }

    fn init_repo() -> tempfile::TempDir {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path();
        git(dir, &["init", "-q", "-b", "main"]);
        git(dir, &["config", "user.email", "test@example.com"]);
        git(dir, &["config", "user.name", "Test"]);
        tmp
    }

    #[test]
    fn nth_space_split_stops_after_n_and_keeps_the_rest_verbatim() {
        let (fields, rest) =
            nth_space_split("1 M. N... 100644 100644 100644 hh hh a path.rs", 8).unwrap();
        assert_eq!(
            fields,
            vec!["1", "M.", "N...", "100644", "100644", "100644", "hh", "hh"]
        );
        assert_eq!(rest, "a path.rs");
    }

    #[test]
    fn nth_space_split_returns_none_when_short_of_n_fields() {
        assert!(nth_space_split("only two fields", 5).is_none());
    }

    #[test]
    fn parses_an_ordinary_modified_entry() {
        let raw = b"1 .M N... 100644 100644 100644 hhhhhhh hhhhhhh a.rs\0";
        let entries = parse_porcelain_v2_z(raw);
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].path, "a.rs");
        assert_eq!(entries[0].index, '.');
        assert_eq!(entries[0].worktree, 'M');
        assert!(entries[0].renamed_from.is_none());
    }

    #[test]
    fn parses_an_untracked_entry() {
        let raw = b"? new_file.rs\0";
        let entries = parse_porcelain_v2_z(raw);
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].path, "new_file.rs");
        assert_eq!(entries[0].index, '?');
        assert_eq!(entries[0].worktree, '?');
    }

    #[test]
    fn parses_a_rename_entry_consuming_two_nul_segments() {
        let raw =
            b"2 R. N... 100644 100644 100644 hhhhhhh hhhhhhh R100 new.rs\0old.rs\0? trailer.rs\0";
        let entries = parse_porcelain_v2_z(raw);
        assert_eq!(entries.len(), 2, "got {entries:?}");
        assert_eq!(entries[0].path, "new.rs");
        assert_eq!(entries[0].index, 'R');
        assert_eq!(entries[0].worktree, '.');
        assert_eq!(entries[0].renamed_from.as_deref(), Some("old.rs"));
        // The trailing `?` entry must still parse correctly — proves the
        // rename record consumed EXACTLY two segments, not more/fewer.
        assert_eq!(entries[1].path, "trailer.rs");
        assert_eq!(entries[1].index, '?');
    }

    #[test]
    fn empty_output_is_clean() {
        assert!(parse_porcelain_v2_z(b"").is_empty());
    }

    #[test]
    fn status_index_reports_untracked_and_modified_against_a_real_repo() {
        let tmp = init_repo();
        let dir = tmp.path();
        std::fs::write(dir.join("a.txt"), "x\n").unwrap();
        git(dir, &["add", "-A"]);
        git(dir, &["commit", "-q", "-m", "c1"]);
        std::fs::write(dir.join("a.txt"), "changed\n").unwrap();
        std::fs::write(dir.join("untracked.txt"), "new\n").unwrap();

        let store_tmp = tempfile::tempdir().unwrap();
        let store = Store::open(&store_tmp.path().join("index.db")).unwrap();
        let repo_id = store.upsert_repo("r", dir.to_str().unwrap()).unwrap();

        let index = StatusIndex::new();
        let status = index.status(&store, repo_id, dir).unwrap();
        assert!(status.dirty);
        let paths: std::collections::HashSet<&str> =
            status.paths.iter().map(|e| e.path.as_str()).collect();
        assert!(paths.contains("a.txt"));
        assert!(paths.contains("untracked.txt"));
    }

    #[test]
    fn status_index_reports_clean_for_a_committed_tree() {
        let tmp = init_repo();
        let dir = tmp.path();
        std::fs::write(dir.join("a.txt"), "x\n").unwrap();
        git(dir, &["add", "-A"]);
        git(dir, &["commit", "-q", "-m", "c1"]);

        let store_tmp = tempfile::tempdir().unwrap();
        let store = Store::open(&store_tmp.path().join("index.db")).unwrap();
        let repo_id = store.upsert_repo("r", dir.to_str().unwrap()).unwrap();

        let index = StatusIndex::new();
        let status = index.status(&store, repo_id, dir).unwrap();
        assert!(!status.dirty);
        assert!(status.paths.is_empty());
    }

    #[test]
    fn status_index_rebuilds_only_after_a_generation_bump() {
        let tmp = init_repo();
        let dir = tmp.path();
        std::fs::write(dir.join("a.txt"), "x\n").unwrap();
        git(dir, &["add", "-A"]);
        git(dir, &["commit", "-q", "-m", "c1"]);

        let store_tmp = tempfile::tempdir().unwrap();
        let store = Store::open(&store_tmp.path().join("index.db")).unwrap();
        let repo_id = store.upsert_repo("r", dir.to_str().unwrap()).unwrap();

        let index = StatusIndex::new();
        let first = index.status(&store, repo_id, dir).unwrap();
        assert!(!first.dirty);

        // Dirty the tree WITHOUT bumping the store's generation — the
        // cached (clean) snapshot must still be served.
        std::fs::write(dir.join("a.txt"), "changed\n").unwrap();
        let cached = index.status(&store, repo_id, dir).unwrap();
        assert!(!cached.dirty, "must still be served from cache");

        // Now bump the generation (any files-table mutation does) — the
        // NEXT call must re-shell and see the real dirty state.
        store
            .upsert_file(repo_id, "b.rs", "hashB", "rust", 1)
            .unwrap();
        let fresh = index.status(&store, repo_id, dir).unwrap();
        assert!(fresh.dirty, "must rebuild once the generation has moved");
    }

    fn fixture_worktree() -> tempfile::TempDir {
        let tmp = init_repo();
        let dir = tmp.path();
        std::fs::create_dir_all(dir.join("src/nested")).unwrap();
        std::fs::write(dir.join("src/lib.rs"), "// lib\n").unwrap();
        std::fs::write(dir.join("src/nested/deep.rs"), "// deep\n").unwrap();
        std::fs::write(dir.join("README.md"), "# hi\n").unwrap();
        git(dir, &["add", "-A"]);
        git(dir, &["commit", "-q", "-m", "c1"]);
        // Untracked-but-not-ignored — must appear.
        std::fs::write(dir.join("src/untracked.rs"), "// new\n").unwrap();
        // .gitignore'd — must NOT appear.
        std::fs::write(dir.join(".gitignore"), "ignored.rs\n").unwrap();
        std::fs::write(dir.join("src/ignored.rs"), "// ignored\n").unwrap();
        git(dir, &["add", ".gitignore"]);
        git(dir, &["commit", "-q", "-m", "c2 gitignore"]);
        // Tracked-but-deleted-from-disk — must NOT appear.
        std::fs::write(dir.join("src/gone.rs"), "// gone\n").unwrap();
        git(dir, &["add", "src/gone.rs"]);
        git(dir, &["commit", "-q", "-m", "c3 add gone"]);
        std::fs::remove_file(dir.join("src/gone.rs")).unwrap();
        tmp
    }

    #[test]
    fn list_worktree_dir_collapses_to_one_level_and_dedups_dirs() {
        let tmp = fixture_worktree();
        let entries = list_worktree_dir(tmp.path(), "").unwrap();
        let got: Vec<(String, &str)> = entries.into_iter().map(|e| (e.name, e.kind)).collect();
        assert_eq!(
            got,
            vec![
                (".gitignore".to_string(), "file"),
                ("README.md".to_string(), "file"),
                ("src".to_string(), "dir"),
            ],
            "got {got:?}"
        );
    }

    #[test]
    fn list_worktree_dir_descends_into_a_named_subdir() {
        let tmp = fixture_worktree();
        let entries = list_worktree_dir(tmp.path(), "src").unwrap();
        let got: Vec<(String, &str)> = entries.into_iter().map(|e| (e.name, e.kind)).collect();
        assert_eq!(
            got,
            vec![
                ("lib.rs".to_string(), "file"),
                ("nested".to_string(), "dir"),
                ("untracked.rs".to_string(), "file"),
            ],
            "must include the untracked file, exclude the gitignored and \
             deleted-from-disk ones, and dedup 'nested' to one dir entry: {got:?}"
        );
    }

    #[test]
    fn list_worktree_dir_omits_a_tracked_but_deleted_from_disk_file() {
        let tmp = fixture_worktree();
        let entries = list_worktree_dir(tmp.path(), "src").unwrap();
        assert!(
            !entries.iter().any(|e| e.name == "gone.rs"),
            "got {entries:?}"
        );
    }
}
