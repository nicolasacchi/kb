//! `GET /api/compare` — repo-level compare (Phase C2's server half): the
//! commit list + merged file stats between two revspecs, two-dot
//! (`from..to`) or three-dot (`from...to`).
//!
//! `git diff` supports BOTH dot forms NATIVELY (`git diff A...B` computes
//! the diff between `merge-base(A,B)` and `B` on its own) — so the file
//! list needs no manual merge-base substitution. The COMMIT LIST does,
//! though: `git log`/`git rev-list` don't have an equivalent "three-dot"
//! shorthand, so a three-dot compare's commit list is spelled out as
//! `merge_base..to` (the conventional "commits unique to `to`" reading a
//! GitHub-style PR compare shows) — computed via a SEPARATE `git
//! merge-base` call, whose result the response also surfaces directly
//! (`resolved.merge_base`).
//!
//! `resolved.from_sha`/`to_sha` are the full, disambiguated shas each side
//! resolves to — read via their own `git rev-parse --verify` call so a
//! caller can see exactly what was resolved even when the range itself is
//! empty (identical refs) or the diff/log calls below never had reason to
//! print either sha on their own.

use super::{
    diff_files, merge_base, parse_log_summary_line, resolve_sha, run_git_raw, totals_for,
    CommitSummary, FileTotals, Resolved, Result, LOG_SUMMARY_FMT,
};
use crate::git::Revspec;
use crate::numstat::FileChange;
use std::path::Path;

/// Commit-list cap — `commits_truncated` is set once more than this many
/// exist; mirrors `routes::search::MAX_SYMBOL_MATCHES`'s own "plain fixed
/// ceiling, not a paginated surface" convention.
pub const MAX_COMMITS: usize = 200;

#[derive(Debug, Clone, serde::Serialize)]
pub struct Compare {
    pub resolved: Resolved,
    /// Newest-first.
    pub commits: Vec<CommitSummary>,
    pub commits_truncated: bool,
    pub files: Vec<FileChange>,
    pub totals: FileTotals,
}

/// `git log --format=<LOG_SUMMARY_FMT> -n <limit+1> <revspec>` —
/// newest-first (git log's own default order), `+1` so the caller can tell
/// "there were MORE than `limit`" apart from "exactly `limit`."
fn commit_list(
    repo_root: &Path,
    revspec: &str,
    limit: usize,
) -> Result<(Vec<CommitSummary>, bool)> {
    let n = (limit + 1).to_string();
    let out = run_git_raw(
        repo_root,
        &[
            "log",
            &format!("--format={LOG_SUMMARY_FMT}"),
            "-n",
            &n,
            revspec,
        ],
    )?;
    let text = String::from_utf8_lossy(&out);
    let mut rows: Vec<CommitSummary> = text.lines().filter_map(parse_log_summary_line).collect();
    let truncated = rows.len() > limit;
    rows.truncate(limit);
    Ok((rows, truncated))
}

/// `GET /api/compare`'s business logic — see the module doc. `from`/`to`
/// get the SAME dash-prefix injection guard `diff::diff_file` uses (both
/// are forwarded to `git diff`/`git log`/`git merge-base`'s raw argv);
/// `..`-free validation is deliberately NOT applied (a revspec may
/// legitimately contain `~`/`^`). Identical `from`/`to` degrade to empty
/// `commits`/`files` through ordinary git behaviour (an empty range), not
/// a special case here.
pub fn compare(repo_root: &Path, from: &Revspec, to: &Revspec, three_dot: bool) -> Result<Compare> {
    // V70-A2 (SEC-17) — validated at the type boundary; the two
    // `reject_dash_prefixed` calls that used to open this fn are subsumed
    // by `Revspec`'s only constructor.
    let (from, to) = (from.as_str(), to.as_str());
    let from_sha = resolve_sha(repo_root, from)?;
    let to_sha = resolve_sha(repo_root, to)?;
    let base = merge_base(repo_root, &from_sha, &to_sha);

    // The commit-list range: `from..to` (two-dot) or `merge_base..to`
    // (three-dot). Disjoint histories (`base == None`) fall back to
    // `from..to` too — still the honest "commits not reachable from
    // `from`" reading `git log` gives, absent a merge-base to range from
    // instead.
    let list_range = match (three_dot, &base) {
        (true, Some(b)) => format!("{b}..{to_sha}"),
        _ => format!("{from_sha}..{to_sha}"),
    };
    let (commits, commits_truncated) = commit_list(repo_root, &list_range, MAX_COMMITS)?;

    // The file diff: `git diff` supports BOTH dot forms natively — no
    // manual merge-base substitution needed here (see the module doc).
    let diff_range = if three_dot {
        format!("{from}...{to}")
    } else {
        format!("{from}..{to}")
    };
    let files = diff_files(repo_root, "diff", &["-M", &diff_range])?;
    let totals = totals_for(&files);

    Ok(Compare {
        resolved: Resolved {
            from_sha,
            to_sha,
            merge_base: base,
        },
        commits,
        commits_truncated,
        files,
        totals,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

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

    /// `main` gets 1 base commit + 1 main-only commit; `feature` (branched
    /// off the base) gets 2 feature-only commits — the standard
    /// "diverged" fixture every branches/compare test in this phase reuses.
    fn diverged_fixture() -> (tempfile::TempDir, String, String, String) {
        let tmp = init_repo();
        let dir = tmp.path();
        std::fs::write(dir.join("a.txt"), "base\n").unwrap();
        git(dir, &["add", "-A"]);
        git(dir, &["commit", "-q", "-m", "base"]);
        let base_sha = git_out(dir, &["rev-parse", "HEAD"]);
        git(dir, &["branch", "feature"]);
        git(dir, &["checkout", "-q", "feature"]);
        std::fs::write(dir.join("b.txt"), "f1\n").unwrap();
        git(dir, &["add", "-A"]);
        git(dir, &["commit", "-q", "-m", "feature one"]);
        std::fs::write(dir.join("c.txt"), "f2\n").unwrap();
        git(dir, &["add", "-A"]);
        git(dir, &["commit", "-q", "-m", "feature two"]);
        let feature_sha = git_out(dir, &["rev-parse", "HEAD"]);
        git(dir, &["checkout", "-q", "main"]);
        std::fs::write(dir.join("d.txt"), "m1\n").unwrap();
        git(dir, &["add", "-A"]);
        git(dir, &["commit", "-q", "-m", "main one"]);
        let main_sha = git_out(dir, &["rev-parse", "HEAD"]);
        (tmp, base_sha, feature_sha, main_sha)
    }

    #[test]
    fn compare_two_dot_lists_only_commits_on_to_not_reachable_from_from() {
        let (tmp, _base, feature_sha, main_sha) = diverged_fixture();
        let dir = tmp.path();
        let cmp = compare(dir, &rs("main"), &rs("feature"), false).unwrap();
        assert_eq!(cmp.resolved.to_sha, feature_sha);
        assert_eq!(cmp.resolved.from_sha, main_sha);
        assert!(cmp.resolved.merge_base.is_some());
        let subjects: Vec<&str> = cmp.commits.iter().map(|c| c.subject.as_str()).collect();
        assert_eq!(subjects, vec!["feature two", "feature one"]);
    }

    #[test]
    fn compare_three_dot_ranges_from_the_merge_base() {
        let (tmp, base_sha, _feature_sha, _main_sha) = diverged_fixture();
        let dir = tmp.path();
        let cmp = compare(dir, &rs("main"), &rs("feature"), true).unwrap();
        assert_eq!(cmp.resolved.merge_base.as_deref(), Some(base_sha.as_str()));
        let subjects: Vec<&str> = cmp.commits.iter().map(|c| c.subject.as_str()).collect();
        assert_eq!(subjects, vec!["feature two", "feature one"]);
        let paths: Vec<&str> = cmp.files.iter().map(|f| f.path.as_str()).collect();
        assert!(paths.contains(&"b.txt"));
        assert!(paths.contains(&"c.txt"));
        assert!(
            !paths.contains(&"d.txt"),
            "three-dot must not include main-only changes: {paths:?}"
        );
    }

    #[test]
    fn compare_identical_refs_yields_empty_commits_and_files() {
        let (tmp, _base, _feature_sha, _main_sha) = diverged_fixture();
        let dir = tmp.path();
        let cmp = compare(dir, &rs("main"), &rs("main"), false).unwrap();
        assert!(cmp.commits.is_empty());
        assert!(cmp.files.is_empty());
        assert_eq!(cmp.totals.files, 0);
    }

    /// V70-A2 (SEC-17): `compare` takes `Revspec`s, so a dash-prefixed
    /// revspec cannot reach it — the refusal happens in the ONE validator
    /// the type's constructor is, and folds into the same `BadRevspec`
    /// 400 this test asserted before.
    #[test]
    fn compare_rejects_a_dash_prefixed_from() {
        let err: super::super::HistoryError = Revspec::parse("--output=/tmp/x").unwrap_err().into();
        assert!(
            matches!(err, super::super::HistoryError::BadRevspec(_)),
            "got: {err:?}"
        );
    }

    #[test]
    fn compare_rejects_a_dash_prefixed_to() {
        let err: super::super::HistoryError = Revspec::parse("-U9999").unwrap_err().into();
        assert!(
            matches!(err, super::super::HistoryError::BadRevspec(_)),
            "got: {err:?}"
        );
    }

    #[test]
    fn compare_reports_file_totals_across_the_range() {
        let (tmp, ..) = diverged_fixture();
        let dir = tmp.path();
        let cmp = compare(dir, &rs("main"), &rs("feature"), false).unwrap();
        assert_eq!(cmp.totals.files, cmp.files.len());
        assert_eq!(
            cmp.totals.insertions,
            cmp.files.iter().map(|f| f.insertions).sum::<u32>()
        );
    }
}
