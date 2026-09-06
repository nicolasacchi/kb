//! `GET /api/repo-state` (Phase G-server) — one repo's CURRENT git
//! operation state: which of rebase/merge/cherry-pick/bisect (if any) is
//! in flight, op-specific detail, every currently-unmerged (conflicted)
//! path, and plain working-tree dirtiness.
//!
//! The op/detail classification is entirely `crate::mirror::gate::
//! detect_op` — this module adds NO second copy of `MARKER_NAMES` or the
//! marker-existence checks; see that fn's doc for the marker-file grammar
//! and the deterministic tie-break order. This module's own job is just
//! the two remaining pieces `detect_op` doesn't cover: the unmerged-path
//! listing and the dirty check, both plain git subprocess calls, same
//! "this crate's Nth git-subprocess wrapper, own error enum" convention as
//! `checkout`/`history`/`diff` before it (see `diff.rs`'s module doc, "Why
//! `DiffError` isn't shared with its siblings," for the shared rationale).

use crate::mirror::{detect_op, OpDetail, RepoOp};
use std::path::Path;
use std::process::Command;

#[derive(Debug, thiserror::Error)]
pub enum RepoStateError {
    #[error("failed to spawn git: {0}")]
    Spawn(std::io::Error),
    #[error("git failed (exit {status}): {stderr}")]
    GitFailed { status: i32, stderr: String },
}

pub type Result<T> = std::result::Result<T, RepoStateError>;

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct RepoState {
    pub op: RepoOp,
    pub detail: OpDetail,
    /// Every currently-unmerged path (`git diff --name-only
    /// --diff-filter=U`), repo-relative — populated regardless of `op`
    /// (an operator can leave conflict markers staged-but-unresolved after
    /// aborting a rebase in a way that clears the marker directory but not
    /// the index, however rare); ordinarily non-empty exactly when `op !=
    /// none`.
    pub conflicted: Vec<String>,
    /// `git status --porcelain` is non-empty — the SAME signal
    /// `checkout::dirty_paths` computes, but this route only needs the
    /// boolean (the caller already has `POST /api/checkout`'s own
    /// structured 409 for the full path list when it actually attempts a
    /// mutation).
    pub dirty: bool,
}

fn run_git(repo_root: &Path, args: &[&str]) -> Result<String> {
    let out = Command::new("git")
        .arg("-C")
        .arg(repo_root)
        .args(args)
        .output()
        .map_err(RepoStateError::Spawn)?;
    if !out.status.success() {
        return Err(RepoStateError::GitFailed {
            status: out.status.code().unwrap_or(-1),
            stderr: String::from_utf8_lossy(&out.stderr).trim().to_string(),
        });
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

fn conflicted_paths(repo_root: &Path) -> Result<Vec<String>> {
    let out = run_git(repo_root, &["diff", "--name-only", "--diff-filter=U"])?;
    Ok(out
        .lines()
        .filter(|l| !l.trim().is_empty())
        .map(|s| s.to_string())
        .collect())
}

/// `pub(crate)` — DCB W1.C's doc-lens reports `dirty` per repo on both the
/// lens and the scorecard, and reuses THIS invocation rather than growing a
/// second `git status --porcelain` call site with its own subtly different
/// flags. Blocking (a subprocess): callers must already be inside
/// `spawn_blocking`.
pub(crate) fn is_dirty(repo_root: &Path) -> Result<bool> {
    let out = run_git(repo_root, &["status", "--porcelain"])?;
    Ok(!out.trim().is_empty())
}

/// `GET /api/repo-state`'s business logic — `git_dir` is the repo's own
/// gitdir (`GitRepo::git_dir()`, resolved by the caller before this runs;
/// see `routes::repo_state_route`'s own doc for why that resolve happens
/// OUTSIDE the `spawn_blocking` this fn itself runs inside).
pub fn repo_state(repo_root: &Path, git_dir: &Path) -> Result<RepoState> {
    let (op, detail) = detect_op(git_dir);
    let conflicted = conflicted_paths(repo_root)?;
    let dirty = is_dirty(repo_root)?;
    Ok(RepoState {
        op,
        detail,
        conflicted,
        dirty,
    })
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

    fn init_repo() -> tempfile::TempDir {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path();
        git(dir, &["init", "-q", "-b", "main"]);
        git(dir, &["config", "user.email", "test@example.com"]);
        git(dir, &["config", "user.name", "Test"]);
        tmp
    }

    #[test]
    fn repo_state_reports_none_and_clean_for_an_ordinary_repo() {
        let tmp = init_repo();
        let dir = tmp.path();
        std::fs::write(dir.join("a.txt"), "x\n").unwrap();
        git(dir, &["add", "-A"]);
        git(dir, &["commit", "-q", "-m", "c1"]);

        let git_dir = dir.join(".git");
        let rs = repo_state(dir, &git_dir).unwrap();
        assert_eq!(rs.op, RepoOp::None);
        assert!(rs.conflicted.is_empty());
        assert!(!rs.dirty);
    }

    #[test]
    fn repo_state_reports_dirty_for_an_uncommitted_edit() {
        let tmp = init_repo();
        let dir = tmp.path();
        std::fs::write(dir.join("a.txt"), "x\n").unwrap();
        git(dir, &["add", "-A"]);
        git(dir, &["commit", "-q", "-m", "c1"]);
        std::fs::write(dir.join("a.txt"), "changed\n").unwrap();

        let git_dir = dir.join(".git");
        let rs = repo_state(dir, &git_dir).unwrap();
        assert!(rs.dirty);
        assert_eq!(rs.op, RepoOp::None);
    }

    #[test]
    fn repo_state_reports_merge_op_and_conflicted_paths_during_a_real_conflict() {
        let tmp = init_repo();
        let dir = tmp.path();
        std::fs::write(dir.join("a.txt"), "base\n").unwrap();
        git(dir, &["add", "-A"]);
        git(dir, &["commit", "-q", "-m", "base"]);
        git(dir, &["branch", "feature"]);
        git(dir, &["checkout", "-q", "feature"]);
        std::fs::write(dir.join("a.txt"), "feature\n").unwrap();
        git(dir, &["add", "-A"]);
        git(dir, &["commit", "-q", "-m", "feature edits a"]);
        git(dir, &["checkout", "-q", "main"]);
        std::fs::write(dir.join("a.txt"), "main\n").unwrap();
        git(dir, &["add", "-A"]);
        git(dir, &["commit", "-q", "-m", "main edits a"]);

        // Start a real conflicting merge and leave it mid-flight.
        let merge_out = StdCommand::new("git")
            .arg("-C")
            .arg(dir)
            .args(["merge", "feature"])
            .output()
            .unwrap();
        assert!(
            !merge_out.status.success(),
            "the merge must actually conflict for this test to mean anything"
        );

        let git_dir = dir.join(".git");
        let rs = repo_state(dir, &git_dir).unwrap();
        assert_eq!(rs.op, RepoOp::Merge);
        assert!(rs.detail.head_sha.is_some());
        assert_eq!(rs.conflicted, vec!["a.txt".to_string()]);
        assert!(rs.dirty);

        // Cleanup: abort the in-flight merge so this fixture's tempdir
        // doesn't leave a lingering merge state (harmless either way since
        // it's a throwaway tempdir, but tidy).
        StdCommand::new("git")
            .arg("-C")
            .arg(dir)
            .args(["merge", "--abort"])
            .status()
            .unwrap();
    }
}
