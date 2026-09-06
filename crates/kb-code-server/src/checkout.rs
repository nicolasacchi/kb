//! W4.7 — confirmed checkout (wave-4 operator ruling: the first
//! sanctioned working-tree mutation this daemon performs; V4.S1's
//! suggestion apply is the second). `switch_repo` refuses
//! (structured, listing every dirty path) on a dirty working tree; a clean
//! tree gets `git switch <ref>` for a known local branch, or `git checkout
//! <ref>` for anything else (tag, remote branch, or a raw sha — `git
//! switch` refuses those without an explicit `--detach`, whereas plain
//! `git checkout` has always detached HEAD for them, exactly matching what
//! an operator typing the equivalent command at a terminal would get).
//!
//! Deliberately owns NO store/mirror wiring: the existing live-mirror
//! watcher (`crate::mirror`) already treats a plain (non-marker) `git
//! checkout`'s HEAD move as an ordinary idle `GitDirSignal::HeadCandidate`
//! (see that module's doc, "A plain (non-marker) operation like `git
//! checkout`" — and `tests/mirror_matrix.rs`'s own
//! `checkout_produces_head_moved_and_one_reconcile_matching_diff` case
//! already pins exactly this for a raw `git checkout` subprocess), so
//! `switch_repo` below is nothing more than the subprocess call itself —
//! the daemon's already-armed watcher picks up the resulting HEAD move and
//! working-tree delta on its own, with no special-casing required.
//!
//! `CheckoutError` is its own enum, deliberately NOT shared with this
//! crate's other five git-subprocess wrappers (`diff::DiffError`,
//! `blame::BlameError`, `sessiondiff::git_diff::DiffError`,
//! `history::HistoryError`, `mirror::reconcile`'s error-less subprocess
//! call) — see `diff.rs`'s
//! module doc ("Why `DiffError` isn't shared with its siblings") for the
//! full rationale. This module's own variant that only ITS callers need:
//! `Dirty`, the refuse-on-a-dirty-tree guard `POST /api/checkout` maps to a
//! structured 409 body (`routes.rs`) — a shape no sibling wrapper has any
//! use for.

use crate::git::Revspec;
use std::path::Path;
use std::process::Command;

#[derive(Debug, thiserror::Error)]
pub enum CheckoutError {
    /// The working tree has uncommitted changes — every dirty path, exactly
    /// as `git status --porcelain` reports it (the 2-character status
    /// prefix stripped).
    #[error("working tree is dirty ({} path(s))", .0.len())]
    Dirty(Vec<String>),
    #[error("failed to spawn git: {0}")]
    Spawn(std::io::Error),
    #[error("git {} failed: {stderr}", .args.join(" "))]
    GitFailed { args: Vec<String>, stderr: String },
    /// `target` reached here starting with `-` — passed as a raw argv entry
    /// to `git switch`/`git checkout`, where a leading dash makes git
    /// option-parse it rather than treat it as a ref, even though this
    /// spawn never goes through a shell (same injection class as
    /// `diff::DiffError::BadRevspec`). `POST /api/checkout` is already
    /// mounted on the LOOPBACK-ONLY sub-router (`router.rs`), so this is
    /// belt-and-suspenders rather than the primary defense — but rejected
    /// before `is_local_branch`/`run_git` ever see it regardless.
    #[error("ref must not start with '-': {0:?}")]
    BadTarget(String),
}

pub type Result<T> = std::result::Result<T, CheckoutError>;

/// What actually happened — the route's 200 body.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CheckoutOutcome {
    /// The ref as given by the caller (branch name, tag, or sha).
    pub target: String,
    /// `true` when this landed on a detached HEAD (anything that isn't a
    /// known local branch).
    pub detached: bool,
}

/// `git status --porcelain` — every dirty path (staged, unstaged, or
/// untracked), repo-relative, with the 2-character porcelain status prefix
/// stripped. Empty means a clean working tree. Deliberately no further
/// parsing of the status codes themselves (mirrors `blame`/`sessiondiff`'s
/// own git subprocess wrappers: pass the caller everything, don't
/// over-interpret) — a rename's `old -> new` form is kept verbatim.
pub fn dirty_paths(repo_root: &Path) -> Result<Vec<String>> {
    let out = run_git(repo_root, &["status", "--porcelain"])?;
    Ok(out
        .lines()
        .filter(|l| !l.trim().is_empty())
        .map(|l| l.get(3..).unwrap_or(l).trim().to_string())
        .collect())
}

/// `true` if `target` names a local branch (`refs/heads/<target>`) in this
/// repo — decides `switch` vs `checkout` below.
fn is_local_branch(repo_root: &Path, target: &str) -> bool {
    Command::new("git")
        .arg("-C")
        .arg(repo_root)
        .args([
            "show-ref",
            "--verify",
            "--quiet",
            &format!("refs/heads/{target}"),
        ])
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

/// Refuse (`CheckoutError::Dirty`) on a dirty working tree; otherwise run
/// `git switch <target>` (known local branch) or `git checkout <target>`
/// (anything else) — see the module doc for the full contract.
///
/// V70-A2 (SEC-17) — `target` is a [`Revspec`], the type whose only
/// constructor is the validator. This is the daemon's FIRST sanctioned
/// working-tree mutation, so it is exactly the call the critique means by
/// "make it a type, not a habit": the old inline `starts_with('-')` guard
/// is gone, and an unvalidated `String` can no longer reach this fn at
/// all. `CheckoutError::BadTarget` survives as the wire shape the route
/// maps a `RevspecError` to, so the refusal a caller sees is unchanged.
pub fn switch_repo(repo_root: &Path, target: &Revspec) -> Result<CheckoutOutcome> {
    let target = target.as_str();
    let dirty = dirty_paths(repo_root)?;
    if !dirty.is_empty() {
        return Err(CheckoutError::Dirty(dirty));
    }
    let detached = !is_local_branch(repo_root, target);
    let verb = if detached { "checkout" } else { "switch" };
    run_git(repo_root, &[verb, target])?;
    Ok(CheckoutOutcome {
        target: target.to_string(),
        detached,
    })
}

/// V70-A2 (SEC-17) — a `RevspecError` from the route's own
/// `Revspec::parse` folds into this module's `BadTarget`, so a rejected
/// `POST /api/checkout` body renders byte-identically to the pre-V70-A2
/// inline guard's refusal.
impl From<crate::git::RevspecError> for CheckoutError {
    fn from(e: crate::git::RevspecError) -> Self {
        CheckoutError::BadTarget(e.0)
    }
}

fn run_git(repo_root: &Path, args: &[&str]) -> Result<String> {
    let out = Command::new("git")
        .arg("-C")
        .arg(repo_root)
        .args(args)
        .output()
        .map_err(CheckoutError::Spawn)?;
    if !out.status.success() {
        return Err(CheckoutError::GitFailed {
            args: args.iter().map(|s| s.to_string()).collect(),
            stderr: String::from_utf8_lossy(&out.stderr).trim().to_string(),
        });
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Test-local shorthand: every fixture target here is a branch name or
    /// sha this test just minted, so `parse` cannot fail.
    fn rs(s: &str) -> Revspec {
        Revspec::parse(s).expect("fixture revspec parses")
    }
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

    fn init_repo(dir: &Path) {
        git(dir, &["init", "-q", "-b", "main"]);
        git(dir, &["config", "user.email", "test@example.com"]);
        git(dir, &["config", "user.name", "Test"]);
    }

    fn fixture_two_branches() -> tempfile::TempDir {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path();
        init_repo(dir);
        std::fs::write(dir.join("a.txt"), "base\n").unwrap();
        git(dir, &["add", "-A"]);
        git(dir, &["commit", "-q", "-m", "base"]);
        git(dir, &["branch", "feature"]);
        tmp
    }

    #[test]
    fn dirty_paths_is_empty_on_a_clean_tree() {
        let tmp = fixture_two_branches();
        assert!(dirty_paths(tmp.path()).unwrap().is_empty());
    }

    #[test]
    fn dirty_paths_lists_untracked_and_modified_files() {
        let tmp = fixture_two_branches();
        let dir = tmp.path();
        std::fs::write(dir.join("a.txt"), "modified\n").unwrap();
        std::fs::write(dir.join("untracked.txt"), "new\n").unwrap();
        let mut dirty = dirty_paths(dir).unwrap();
        dirty.sort();
        assert_eq!(
            dirty,
            vec!["a.txt".to_string(), "untracked.txt".to_string()]
        );
    }

    #[test]
    fn switch_repo_refuses_on_a_dirty_tree_listing_every_path() {
        let tmp = fixture_two_branches();
        let dir = tmp.path();
        std::fs::write(dir.join("a.txt"), "modified\n").unwrap();
        let err = switch_repo(dir, &rs("feature")).unwrap_err();
        match err {
            CheckoutError::Dirty(paths) => assert_eq!(paths, vec!["a.txt".to_string()]),
            other => panic!("expected Dirty, got {other:?}"),
        }
        // HEAD must not have moved.
        let head = git_head(dir);
        assert_eq!(head, "main");
    }

    #[test]
    fn switch_repo_rejects_a_dash_prefixed_target_without_moving_head() {
        let tmp = fixture_two_branches();
        let dir = tmp.path();
        // V70-A2: the refusal moved INTO the type this fn takes, so a
        // dash-prefixed target is unreachable by construction; the
        // route-visible error is the same `BadTarget` as before.
        let err: CheckoutError = Revspec::parse("--help").unwrap_err().into();
        assert!(matches!(err, CheckoutError::BadTarget(_)), "got: {err:?}");
        // ...and HEAD is where it was: nothing ran.
        assert_eq!(git_head(dir), "main");
    }

    #[test]
    fn switch_repo_switches_a_clean_tree_to_a_local_branch() {
        let tmp = fixture_two_branches();
        let dir = tmp.path();
        let outcome = switch_repo(dir, &rs("feature")).unwrap();
        assert_eq!(outcome.target, "feature");
        assert!(!outcome.detached);
        assert_eq!(git_head(dir), "feature");
    }

    #[test]
    fn switch_repo_detaches_for_a_raw_sha() {
        let tmp = fixture_two_branches();
        let dir = tmp.path();
        let sha = String::from_utf8(
            StdCommand::new("git")
                .arg("-C")
                .arg(dir)
                .args(["rev-parse", "HEAD"])
                .output()
                .unwrap()
                .stdout,
        )
        .unwrap()
        .trim()
        .to_string();
        let outcome = switch_repo(dir, &rs(&sha)).unwrap();
        assert!(outcome.detached);
        // Detached HEAD: `git symbolic-ref` fails (no branch name).
        let sym = StdCommand::new("git")
            .arg("-C")
            .arg(dir)
            .args(["symbolic-ref", "-q", "--short", "HEAD"])
            .output()
            .unwrap();
        assert!(!sym.status.success(), "HEAD must be detached");
    }

    #[test]
    fn switch_repo_reports_a_clean_error_for_an_unknown_ref() {
        let tmp = fixture_two_branches();
        let dir = tmp.path();
        let err = switch_repo(dir, &rs("does-not-exist")).unwrap_err();
        match err {
            CheckoutError::GitFailed { .. } => {}
            other => panic!("expected GitFailed, got {other:?}"),
        }
        // HEAD must still be on main — a failed checkout leaves it alone.
        assert_eq!(git_head(dir), "main");
    }

    fn git_head(dir: &Path) -> String {
        String::from_utf8(
            StdCommand::new("git")
                .arg("-C")
                .arg(dir)
                .args(["symbolic-ref", "--short", "HEAD"])
                .output()
                .unwrap()
                .stdout,
        )
        .unwrap()
        .trim()
        .to_string()
    }
}
