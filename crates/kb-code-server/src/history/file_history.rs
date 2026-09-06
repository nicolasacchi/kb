//! `GET /api/file-history` — per-file history (Phase C4's server half):
//! `git log --follow` for one path, newest-first, optionally bounded by
//! `before` (unix seconds).
//!
//! `path` is assumed already `routes::safe_rel_path`-gated by the caller —
//! no argument-injection guard is needed here even though this module
//! takes no dash-prefix check of its own: `path` is always passed AFTER a
//! `--` pathspec separator, which fully protects it from git's own option
//! parser regardless of a leading `-` (the same reasoning
//! `routes::safe_rel_path`'s own doc gives: THAT gate is about filesystem
//! traversal, not argument injection, because injection is already closed
//! by the `--` convention).

use super::{parse_log_summary_line, run_git_raw, CommitSummary, Result, LOG_SUMMARY_FMT};
use std::path::Path;

pub const DEFAULT_LIMIT: usize = 100;
pub const MAX_LIMIT: usize = 500;

/// `git log --follow --format=<LOG_SUMMARY_FMT> [--before=@<unix>] -n
/// <limit+1> -- <path>` — see the module doc. `limit` is the route's own
/// already-clamped value (`[1, MAX_LIMIT]`); `before_unix` (when given) is
/// passed as git's `@<epoch>` approxidate form.
pub fn file_history(
    repo_root: &Path,
    path: &str,
    limit: usize,
    before_unix: Option<i64>,
) -> Result<(Vec<CommitSummary>, bool)> {
    let fmt_arg = format!("--format={LOG_SUMMARY_FMT}");
    let n = (limit + 1).to_string();
    let before_arg = before_unix.map(|t| format!("--before=@{t}"));

    let mut args: Vec<&str> = vec!["log", "--follow", &fmt_arg, "-n", &n];
    if let Some(b) = before_arg.as_deref() {
        args.push(b);
    }
    args.push("--");
    args.push(path);

    let out = run_git_raw(repo_root, &args)?;
    let text = String::from_utf8_lossy(&out);
    let mut rows: Vec<CommitSummary> = text.lines().filter_map(parse_log_summary_line).collect();
    let truncated = rows.len() > limit;
    rows.truncate(limit);
    Ok((rows, truncated))
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

    fn commit_at(dir: &Path, file: &str, contents: &str, message: &str, unix_secs: i64) {
        std::fs::write(dir.join(file), contents).unwrap();
        git(dir, &["add", "-A"]);
        let date = format!("{unix_secs} +0000");
        let status = StdCommand::new("git")
            .arg("-C")
            .arg(dir)
            .args(["commit", "-q", "-m", message])
            .env("GIT_AUTHOR_DATE", &date)
            .env("GIT_COMMITTER_DATE", &date)
            .status()
            .unwrap();
        assert!(status.success());
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
    fn file_history_follows_a_rename_across_commits() {
        let tmp = init_repo();
        let dir = tmp.path();
        commit_at(dir, "a.txt", "one\n", "c1", 1_700_000_000);
        commit_at(dir, "a.txt", "one\ntwo\n", "c2", 1_700_001_000);
        git(dir, &["mv", "a.txt", "renamed.txt"]);
        let status = StdCommand::new("git")
            .arg("-C")
            .arg(dir)
            .args(["commit", "-q", "-m", "rename it"])
            .env("GIT_AUTHOR_DATE", "1700002000 +0000")
            .env("GIT_COMMITTER_DATE", "1700002000 +0000")
            .status()
            .unwrap();
        assert!(status.success());

        let (entries, truncated) = file_history(dir, "renamed.txt", 100, None).unwrap();
        assert!(!truncated);
        let subjects: Vec<&str> = entries.iter().map(|e| e.subject.as_str()).collect();
        assert_eq!(
            subjects,
            vec!["rename it", "c2", "c1"],
            "--follow must trace history across the rename"
        );
    }

    #[test]
    fn file_history_respects_limit_and_reports_truncation() {
        let tmp = init_repo();
        let dir = tmp.path();
        commit_at(dir, "a.txt", "1\n", "c1", 1_700_000_000);
        commit_at(dir, "a.txt", "2\n", "c2", 1_700_001_000);
        commit_at(dir, "a.txt", "3\n", "c3", 1_700_002_000);

        let (entries, truncated) = file_history(dir, "a.txt", 2, None).unwrap();
        assert_eq!(entries.len(), 2);
        assert!(truncated);
        assert_eq!(entries[0].subject, "c3");
        assert_eq!(entries[1].subject, "c2");
    }

    #[test]
    fn file_history_before_narrows_to_earlier_commits() {
        let tmp = init_repo();
        let dir = tmp.path();
        commit_at(dir, "a.txt", "1\n", "c1", 1_700_000_000);
        commit_at(dir, "a.txt", "2\n", "c2", 1_700_100_000);

        let (entries, truncated) = file_history(dir, "a.txt", 100, Some(1_700_050_000)).unwrap();
        assert!(!truncated);
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].subject, "c1");
    }

    #[test]
    fn file_history_on_a_never_existed_path_is_empty_not_an_error() {
        let tmp = init_repo();
        let dir = tmp.path();
        commit_at(dir, "a.txt", "1\n", "c1", 1_700_000_000);

        let (entries, truncated) = file_history(dir, "never-existed.txt", 100, None).unwrap();
        assert!(entries.is_empty());
        assert!(!truncated);
    }
}
