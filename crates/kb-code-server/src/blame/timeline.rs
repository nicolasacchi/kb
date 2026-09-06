//! Bounded, on-demand line history via `git log -L <line>,<line>:<path>` —
//! the SET-VALUED "every commit that has ever touched this exact line"
//! view, distinct from blame's single "who's responsible for it as the file
//! stands right now" view. ADR-4's own wording: a bounded ON-DEMAND `git log
//! -L` call, not a precomputed/cached structure — a line's touch history is
//! cheap enough to compute, and rare enough to ask for, that it doesn't
//! need `cache`'s (commit, path) reuse story.

use serde::Serialize;
use std::path::Path;
use std::process::Command;

/// Default cap on how many historical entries [`line_timeline`] returns
/// when the caller doesn't override it.
pub const DEFAULT_MAX_ENTRIES: usize = 20;

/// `git log`'s own `--format` string for this feature. A raw SOH (0x01)
/// byte practically never appears in real source/diff text, so prefixing
/// every RECORD line with it safely disambiguates a record from an ordinary
/// unified-diff body line (which `git log -L` also prints, and which this
/// parser otherwise ignores) without needing to understand diff-hunk syntax
/// at all. Fields are ALSO SOH-separated, with `%s` (the free-text subject
/// — the one field that could itself contain almost anything) placed LAST,
/// so parsing never needs to guess where the subject ends.
const FORMAT: &str = "\u{1}%H\u{1}%at\u{1}%s";

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct TimelineEntry {
    pub sha: String,
    /// Unix seconds.
    pub author_time: i64,
    pub subject: String,
}

#[derive(Debug, thiserror::Error)]
pub enum TimelineError {
    #[error("failed to spawn git log: {0}")]
    Spawn(std::io::Error),
    #[error("git log -L failed (exit {status}): {stderr}")]
    GitFailed { status: i32, stderr: String },
    #[error("git log -L produced non-UTF8 output")]
    InvalidUtf8,
}

pub type Result<T> = std::result::Result<T, TimelineError>;

/// The set-valued history of `path`'s `line` (1-based), newest-first,
/// capped at `max_entries` via git's own `-n` — an old, heavily-churned
/// line never walks its FULL history just to answer "what are the last N
/// touches." Blocking: spawns a real subprocess; an async caller MUST wrap
/// this in `spawn_blocking` (see `incremental::run_streaming`'s doc for the
/// same rationale, including `GitRepo`'s `!Send` note — this fn takes a
/// bare `repo_root` rather than a `GitRepo` for exactly that reason, so
/// there's nothing non-`Send` to smuggle across the boundary in the first
/// place).
pub fn line_timeline(
    repo_root: &Path,
    path: &str,
    line: u32,
    max_entries: usize,
) -> Result<Vec<TimelineEntry>> {
    let range = format!("{line},{line}:{path}");
    // `--format <value>` as TWO separate argv entries does NOT work — unlike
    // `-L`, git's `log --format` only accepts the `=`-attached form; passed
    // as two args, git tries to parse the format STRING itself as the next
    // revision/pathspec and fails with "ambiguous argument" (caught by this
    // module's own fixture-repo test, `line_timeline_is_newest_first_and_
    // bounded_by_max_entries` — a real regression once, not a hypothetical).
    let format_arg = format!("--format={FORMAT}");
    let n = max_entries.to_string();
    let output = Command::new("git")
        .arg("-C")
        .arg(repo_root)
        .args(["log", "-L", &range, &format_arg, "-n", &n])
        .output()
        .map_err(TimelineError::Spawn)?;
    if !output.status.success() {
        return Err(TimelineError::GitFailed {
            status: output.status.code().unwrap_or(-1),
            stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
        });
    }
    let text = String::from_utf8(output.stdout).map_err(|_| TimelineError::InvalidUtf8)?;
    Ok(parse_timeline(&text))
}

/// Pure parser (unit-testable without spawning `git`) — pulls every
/// SOH-prefixed record line out of `text` in order, ignoring every other
/// line (the unified-diff hunks `git log -L` also prints, which this
/// feature has no use for).
fn parse_timeline(text: &str) -> Vec<TimelineEntry> {
    text.lines()
        .filter_map(|line| line.strip_prefix('\u{1}'))
        .filter_map(|rest| {
            let mut parts = rest.splitn(3, '\u{1}');
            let sha = parts.next()?;
            let ts = parts.next()?;
            let subject = parts.next().unwrap_or("");
            Some(TimelineEntry {
                sha: sha.to_string(),
                author_time: ts.parse().ok()?,
                subject: subject.to_string(),
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::Command as StdCommand;

    // --- pure parser tests --------------------------------------------------

    #[test]
    fn parse_timeline_extracts_only_record_lines_in_order() {
        let text = "\u{1}sha1\u{1}1000\u{1}first subject\n\
             \n\
             diff --git a/f.txt b/f.txt\n\
             index abc..def 100644\n\
             --- a/f.txt\n\
             +++ b/f.txt\n\
             @@ -1,1 +1,1 @@\n\
             -old line\n\
             +new line\n\
             \u{1}sha2\u{1}2000\u{1}second subject with | a pipe\n";
        let entries = parse_timeline(text);
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].sha, "sha1");
        assert_eq!(entries[0].author_time, 1000);
        assert_eq!(entries[0].subject, "first subject");
        assert_eq!(entries[1].sha, "sha2");
        assert_eq!(entries[1].author_time, 2000);
        assert_eq!(entries[1].subject, "second subject with | a pipe");
    }

    #[test]
    fn parse_timeline_of_empty_text_is_empty() {
        assert_eq!(parse_timeline(""), Vec::new());
    }

    // --- real fixture-repo tests --------------------------------------------

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

    fn commit_as(dir: &Path, name: &str, email: &str, msg: &str) {
        git(dir, &["config", "user.name", name]);
        git(dir, &["config", "user.email", email]);
        git(dir, &["commit", "-q", "-m", msg]);
    }

    /// A file whose FIRST line is touched by every one of five commits, so
    /// `-n` bounding is directly observable.
    fn churn_repo() -> tempfile::TempDir {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path();
        git(dir, &["init", "-q", "-b", "main"]);
        for i in 0..5 {
            std::fs::write(dir.join("f.txt"), format!("version {i}\n")).unwrap();
            git(dir, &["add", "-A"]);
            commit_as(dir, "Author", "a@example.com", &format!("commit {i}"));
        }
        tmp
    }

    #[test]
    fn line_timeline_is_newest_first_and_bounded_by_max_entries() {
        let repo = churn_repo();
        let all = line_timeline(repo.path(), "f.txt", 1, 100).unwrap();
        assert_eq!(all.len(), 5, "every one of the 5 commits touched line 1");
        // Newest-first: author-time strictly descending.
        for pair in all.windows(2) {
            assert!(
                pair[0].author_time >= pair[1].author_time,
                "expected newest-first ordering: {all:?}"
            );
        }
        assert_eq!(all[0].subject, "commit 4");
        assert_eq!(all[4].subject, "commit 0");

        let bounded = line_timeline(repo.path(), "f.txt", 1, 2).unwrap();
        assert_eq!(bounded.len(), 2);
        assert_eq!(bounded[0].subject, "commit 4");
        assert_eq!(bounded[1].subject, "commit 3");
    }

    #[test]
    fn default_max_entries_constant_is_twenty() {
        assert_eq!(DEFAULT_MAX_ENTRIES, 20);
    }

    #[test]
    fn out_of_range_line_reports_a_git_failed_error() {
        let repo = churn_repo();
        let err = line_timeline(repo.path(), "f.txt", 999, 10).unwrap_err();
        match err {
            TimelineError::GitFailed { stderr, .. } => {
                assert!(stderr.to_lowercase().contains("line"), "got: {stderr}");
            }
            other => panic!("expected GitFailed, got: {other:?}"),
        }
    }
}
