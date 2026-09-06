//! `git show --numstat` subprocess wrapper (ADR-4: shell out for diff-shaped
//! work rather than reimplementing git's own diff algorithm — the same
//! precedent `mirror::reconcile::run_git_diff` and `blame::incremental`
//! already established). This module owns exactly the PER-FILE stats a
//! commit's diff produces; it does NOT read author time / subject / parent
//! count — `crate::git::commit_info` (gix, in-process) already covers that
//! metadata (see `join::local::resolve_local`'s precedent for the same
//! split: gix for metadata, a subprocess for diff-shaped work).
//!
//! [`commit_numstat`] is what [`super::session_diff`] actually calls, for
//! every locally-resolved commit, to get its file-level insertion/deletion
//! totals. [`commit_patch`] (a single file's full patch text) is the
//! documented on-demand companion — `session_diff`'s own JSON payload never
//! calls it (a session's full patch text, across every file of every
//! commit, has no natural size bound); it exists here, tested, for a future
//! "show me this one file's diff" affordance (SPA/CLI) to call directly.
//!
//! Both fns are BLOCKING (a real `git` subprocess, synchronous stdout
//! read) — an async caller MUST wrap them in `spawn_blocking`, exactly like
//! `blame::incremental::run_streaming`'s own doc requires.
//!
//! [`parse_numstat_line`]'s counts step (`"N\tM"` → insertions/deletions,
//! `"-\t-"` → binary) funnels through `crate::numstat::parse_counts` — the
//! ONE shared counts-parsing routine, reused by `crate::history`'s own
//! (structurally different, `-z`/NUL-framed) numstat parse for its
//! `GET /api/commit`/`GET /api/compare` file lists. See `crate::numstat`'s
//! module doc for why the surrounding path-consumption logic is NOT
//! shared: this module's own newline/tab-framed form can't tell a rename
//! apart from an ordinary path without `-z` (`crate::history`'s form
//! disambiguates that via a parallel `--name-status` run this module has
//! no need to make), so the two stay separate above the shared counts
//! step.
//!
//! # Why this `DiffError` isn't shared with its siblings
//!
//! Yes, this crate already has a `crate::diff::DiffError` — this module's
//! own, same-named, `DiffError` is a DELIBERATE separate enum, not an
//! accidental near-duplicate (see `diff.rs`'s module doc, "Why `DiffError`
//! isn't shared with its siblings", for the full five-wrapper rationale).
//! This is in fact the NARROWEST of the five: no `BadRevspec` guard,
//! because [`commit_numstat`]'s `sha` argument (`super::resolve_one_commit`,
//! its one caller) is always a full sha already resolved via
//! `crate::git::GitRepo::commit_info` from a `kb_client.session_commits`
//! row — kb-core's OWN session-commit index — never a raw, caller-supplied
//! revspec, so the argument-injection surface `diff::DiffError::BadRevspec`
//! and `checkout::CheckoutError::BadTarget` each guard against doesn't
//! exist here at all.

use std::path::Path;
use std::process::Command;

/// One file's line-delta from a commit's `--numstat` output.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileStat {
    /// Repo-relative, forward-slash (git's own `--numstat` path form).
    pub path: String,
    pub insertions: u32,
    pub deletions: u32,
    /// `true` when git reported `-\t-\t<path>` (a binary file) — git never
    /// counts line deltas for one, so `insertions`/`deletions` are always
    /// `0` in that case; this flag is what distinguishes "binary" from "a
    /// genuinely empty text-file diff" (e.g. a mode-only change).
    pub binary: bool,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CommitNumstat {
    pub files: Vec<FileStat>,
}

#[derive(Debug, thiserror::Error)]
pub enum DiffError {
    #[error("failed to spawn git: {0}")]
    Spawn(std::io::Error),
    #[error("git show failed (exit {status}): {stderr}")]
    GitFailed { status: i32, stderr: String },
}

pub type Result<T> = std::result::Result<T, DiffError>;

fn run_git(repo_root: &Path, args: &[&str]) -> Result<Vec<u8>> {
    let output = Command::new("git")
        .arg("-C")
        .arg(repo_root)
        .args(args)
        .output()
        .map_err(DiffError::Spawn)?;
    if !output.status.success() {
        return Err(DiffError::GitFailed {
            status: output.status.code().unwrap_or(-1),
            stderr: String::from_utf8_lossy(&output.stderr).trim().to_string(),
        });
    }
    Ok(output.stdout)
}

/// `git -C <repo_root> show --numstat --format= <sha>` — the per-file
/// insertion/deletion counts for one commit, no commit-header text (
/// `--format=` blanks it; `crate::git::commit_info` already covers subject/
/// author-time via gix). A merge commit gets git's own default
/// combined-diff numstat (no `-m`/`--first-parent` passed) — rare in the
/// sessions this feature targets, and "something rather than an error" is
/// an accepted v1 scope limit, not specially handled.
pub fn commit_numstat(repo_root: &Path, sha: &str) -> Result<CommitNumstat> {
    let stdout = run_git(repo_root, &["show", "--numstat", "--format=", sha])?;
    let text = String::from_utf8_lossy(&stdout);
    let files = text
        .lines()
        .filter(|l| !l.trim().is_empty())
        .filter_map(parse_numstat_line)
        .collect();
    Ok(CommitNumstat { files })
}

fn parse_numstat_line(line: &str) -> Option<FileStat> {
    let mut parts = line.splitn(3, '\t');
    let ins = parts.next()?;
    let del = parts.next()?;
    let path = parts.next()?.to_string();
    let (insertions, deletions, binary) = crate::numstat::parse_counts(ins, del)?;
    Some(FileStat {
        path,
        insertions,
        deletions,
        binary,
    })
}

/// `git -C <repo_root> show <sha> -- <path>` — one file's full patch text.
/// ON DEMAND ONLY — see the module doc; not called by [`super::session_diff`].
pub fn commit_patch(repo_root: &Path, sha: &str, path: &str) -> Result<String> {
    let stdout = run_git(repo_root, &["show", sha, "--", path])?;
    Ok(String::from_utf8_lossy(&stdout).into_owned())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::Command as StdCommand;

    fn git(dir: &Path, args: &[&str]) {
        let out = StdCommand::new("git")
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

    fn git_out(dir: &Path, args: &[&str]) -> String {
        let out = StdCommand::new("git")
            .arg("-C")
            .arg(dir)
            .args(args)
            .output()
            .expect("git runs");
        assert!(out.status.success());
        String::from_utf8(out.stdout).unwrap().trim().to_string()
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
    fn commit_numstat_reports_insertions_and_deletions_per_file() {
        let tmp = init_repo();
        let dir = tmp.path();
        std::fs::write(dir.join("a.txt"), "line1\nline2\nline3\n").unwrap();
        git(dir, &["add", "a.txt"]);
        git(dir, &["commit", "-q", "-m", "c1"]);

        std::fs::write(dir.join("a.txt"), "line1\nCHANGED\nline3\n").unwrap();
        std::fs::write(dir.join("b.txt"), "new file\n").unwrap();
        git(dir, &["add", "-A"]);
        git(dir, &["commit", "-q", "-m", "c2"]);
        let sha = git_out(dir, &["rev-parse", "HEAD"]);

        let stats = commit_numstat(dir, &sha).unwrap();
        let mut files = stats.files.clone();
        files.sort_by(|a, b| a.path.cmp(&b.path));
        assert_eq!(files.len(), 2);
        assert_eq!(files[0].path, "a.txt");
        assert_eq!(files[0].insertions, 1);
        assert_eq!(files[0].deletions, 1);
        assert!(!files[0].binary);
        assert_eq!(files[1].path, "b.txt");
        assert_eq!(files[1].insertions, 1);
        assert_eq!(files[1].deletions, 0);
    }

    #[test]
    fn commit_numstat_flags_a_binary_file_without_line_counts() {
        let tmp = init_repo();
        let dir = tmp.path();
        std::fs::write(dir.join("bin.dat"), [0u8, 1, 2, 0, 255]).unwrap();
        git(dir, &["add", "bin.dat"]);
        git(dir, &["commit", "-q", "-m", "add binary"]);
        let sha = git_out(dir, &["rev-parse", "HEAD"]);

        let stats = commit_numstat(dir, &sha).unwrap();
        assert_eq!(stats.files.len(), 1);
        assert!(stats.files[0].binary);
        assert_eq!(stats.files[0].insertions, 0);
        assert_eq!(stats.files[0].deletions, 0);
    }

    #[test]
    fn commit_numstat_on_an_unresolvable_sha_errors_cleanly() {
        let tmp = init_repo();
        std::fs::write(tmp.path().join("a.txt"), "x\n").unwrap();
        git(tmp.path(), &["add", "a.txt"]);
        git(tmp.path(), &["commit", "-q", "-m", "c1"]);

        let err = commit_numstat(tmp.path(), "deadbeefdeadbeefdead").unwrap_err();
        assert!(matches!(err, DiffError::GitFailed { .. }));
    }

    #[test]
    fn commit_patch_returns_the_files_diff_text() {
        let tmp = init_repo();
        let dir = tmp.path();
        std::fs::write(dir.join("a.txt"), "line1\n").unwrap();
        git(dir, &["add", "a.txt"]);
        git(dir, &["commit", "-q", "-m", "c1"]);
        std::fs::write(dir.join("a.txt"), "line1\nline2\n").unwrap();
        git(dir, &["add", "a.txt"]);
        git(dir, &["commit", "-q", "-m", "c2"]);
        let sha = git_out(dir, &["rev-parse", "HEAD"]);

        let patch = commit_patch(dir, &sha, "a.txt").unwrap();
        assert!(patch.contains("+line2"), "got: {patch}");
    }
}
