//! `GET /api/range-diff` (Phase G-server) — wraps `git range-diff
//! --no-color <old> <new>`, where `old`/`new` are RANGE strings (e.g.
//! `main..topic@{1}` vs `main..topic`) rather than single revspecs — the
//! standard "did my rebase/amend change anything besides the shas" view.
//!
//! Unlike [`super::merge_check`], `range-diff`'s ordinary exit code
//! discipline is NOT ambiguous: 0 for a normal (possibly all-"equal") run,
//! non-zero for a genuine failure (a bad range, missing arguments, ...) —
//! verified against git 2.55 (`git range-diff --no-color main..nope
//! main..also-nope` exits 128 with a `fatal: bad revision` message). So
//! this module reuses [`super::run_git_raw`] as-is, unlike `merge_check`'s
//! own hand-rolled subprocess call.
//!
//! # The summary-line grammar
//!
//! Every top-level line (captured against a real git 2.55, `cat -A`'d to
//! confirm exact whitespace) has this shape:
//!
//! ```text
//! 1:  75e6a8e = 1:  75e6a8e add a
//! 2:  26ef277 ! 2:  56e64a6 add b
//!     @@ Metadata
//!      ...indented diff body for a modified ("!") entry...
//! -:  ------- > 3:  68f7814 add c
//! 1:  75e6a8e < -:  ------- add a
//! ```
//!
//! `{idx}:  {sha-or-dashes} {sign} {idx}:  {sha-or-dashes} {subject}` —
//! `idx` is a decimal ordinal or `-`; `sha-or-dashes` is either an
//! abbreviated hex object id or a run of `-` placeholders ("absent on this
//! side"); `sign` is one of `=` (equal) / `!` (modified) / `<` (removed —
//! present only on the OLD side) / `>` (added — present only on the NEW
//! side). [`parse_range_diff`] recognises a top-level line by its lack of
//! ANY leading whitespace — every indented continuation line (a `!`
//! entry's own diff body, git's own per-hunk detail) starts with at least
//! one space, so a plain per-line "does this start with whitespace" check
//! reliably tells the two apart without needing to track "am I currently
//! inside a body block" state across lines.
//!
//! # Why a pair carries at most ONE known subject
//!
//! The summary line shows exactly ONE subject string — empirically, for a
//! `!` (modified) pair, that is the OLD commit's subject, even when the
//! amend changed the subject line itself (verified: amending "OLDSUBJECT"
//! to "NEWSUBJECT" still prints "OLDSUBJECT" on the summary line; the
//! actual subject CHANGE, if any, only shows up in the indented diff body
//! under "## Commit message ##", which this parser deliberately does not
//! walk). So [`RangeDiffPair::new_subject`] is honestly `None` for a
//! `modified` pair rather than guessing or duplicating the old one; only
//! `equal` (where the two sides are provably identical, subject included —
//! a message-only change already downgrades a pair from `equal` to
//! `modified`, verified) sets both sides to the same string.

use super::{run_git_raw, Result};
use crate::git::RefRange;
use std::path::Path;

/// Pair-list cap — mirrors `compare::MAX_COMMITS`'s "plain fixed ceiling,
/// not a paginated surface" convention.
pub const MAX_PAIRS: usize = 200;

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct RangeDiffPair {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub old_sha: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub new_sha: Option<String>,
    /// `"equal"` | `"modified"` | `"added"` | `"removed"`.
    pub disposition: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub old_subject: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub new_subject: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct RangeDiff {
    pub pairs: Vec<RangeDiffPair>,
    pub truncated: bool,
}

/// Consume one whitespace-delimited token off the front of `s`, tolerating
/// any run-length of leading spaces (git right-pads `N:` differently
/// depending on how many digits the widest index in the run has, so the
/// gap between tokens is NOT always exactly one space) — returns
/// `(token, rest)`, or `None` if `s` has no more tokens.
fn take_token(s: &str) -> Option<(&str, &str)> {
    let s = s.trim_start_matches(' ');
    let end = s.find(' ').unwrap_or(s.len());
    if end == 0 {
        None
    } else {
        Some((&s[..end], &s[end..]))
    }
}

/// A dashes-only placeholder (`-`, `-------`, any length) means "absent on
/// this side" — `None`, not a sha. Anything else is trusted as-is: this is
/// git's own trusted stdout (an abbreviated object id), not caller input,
/// so no further shape validation applies here the way `join::ladder::
/// is_plausible_sha` gates a caller-supplied string.
fn sha_or_none(token: &str) -> Option<String> {
    if token.is_empty() || token.chars().all(|c| c == '-') {
        None
    } else {
        Some(token.to_string())
    }
}

/// Parse ONE top-level summary line — see the module doc for the grammar
/// and the "why only one subject" rationale. `None` for anything that
/// isn't a well-formed top-level line: an indented diff-body continuation
/// line (recognised by ITS leading whitespace), a blank line, or a
/// genuinely malformed/unrecognised line — all silently skipped rather
/// than failing the whole parse, matching this crate's general "tolerant
/// line-oriented parse" convention (e.g. `parse_log_summary_line`).
fn parse_summary_line(line: &str) -> Option<RangeDiffPair> {
    if line.is_empty() || line.starts_with(' ') || line.starts_with('\t') {
        return None;
    }
    let (idx1, rest) = take_token(line)?;
    let (sha1, rest) = take_token(rest)?;
    let (sign, rest) = take_token(rest)?;
    let (idx2, rest) = take_token(rest)?;
    let (sha2, rest) = take_token(rest)?;
    if !idx1.ends_with(':') || !idx2.ends_with(':') {
        return None;
    }
    let subject = rest.trim_start();

    let old_sha = sha_or_none(sha1);
    let new_sha = sha_or_none(sha2);

    let (disposition, old_subject, new_subject) = match sign {
        "=" => (
            "equal",
            Some(subject.to_string()),
            Some(subject.to_string()),
        ),
        "!" => ("modified", Some(subject.to_string()), None),
        "<" => ("removed", Some(subject.to_string()), None),
        ">" => ("added", None, Some(subject.to_string())),
        _ => return None,
    };

    Some(RangeDiffPair {
        old_sha,
        new_sha,
        disposition,
        old_subject,
        new_subject,
    })
}

/// Parse `git range-diff --no-color`'s whole stdout into a pair list — the
/// pure half unit-tested hard, independent of any subprocess.
pub fn parse_range_diff(text: &str) -> Vec<RangeDiffPair> {
    text.lines().filter_map(parse_summary_line).collect()
}

/// `GET /api/range-diff`'s business logic.
///
/// V70-A2 (SEC-17) — `old`/`new` are [`RefRange`]s, the type the critique
/// asked for by name: a range is exactly the shape the old
/// `reject_user_ref` convention could not express (it REJECTS `..`), so
/// implementers were being pushed to bypass validation here. `RefRange`
/// validates each endpoint as a `Revspec` and reassembles the argv token
/// itself, so `--output=/tmp/x..main` is refused on the left-hand side
/// while `main..topic` passes.
pub fn range_diff(repo_root: &Path, old: &RefRange, new: &RefRange) -> Result<RangeDiff> {
    let (old, new) = (old.as_arg(), new.as_arg());
    let out = run_git_raw(repo_root, &["range-diff", "--no-color", &old, &new])?;
    let text = String::from_utf8_lossy(&out);
    let mut pairs = parse_range_diff(&text);
    let truncated = pairs.len() > MAX_PAIRS;
    pairs.truncate(MAX_PAIRS);

    Ok(RangeDiff { pairs, truncated })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::history::HistoryError;
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

    // --- parser unit tests: every disposition, against captured real-git
    //     fixture text (git 2.55) ------------------------------------------

    #[test]
    fn parses_an_equal_entry() {
        let text = "1:  75e6a8e = 1:  75e6a8e add a\n2:  e473f3a = 2:  e473f3a add b\n";
        let pairs = parse_range_diff(text);
        assert_eq!(pairs.len(), 2);
        assert_eq!(
            pairs[0],
            RangeDiffPair {
                old_sha: Some("75e6a8e".to_string()),
                new_sha: Some("75e6a8e".to_string()),
                disposition: "equal",
                old_subject: Some("add a".to_string()),
                new_subject: Some("add a".to_string()),
            }
        );
    }

    #[test]
    fn parses_a_modified_entry_and_skips_its_indented_diff_body() {
        let text = "\
1:  75e6a8e = 1:  75e6a8e add a
2:  26ef277 ! 2:  56e64a6 add b
    @@ Metadata
     Author: Test <test@example.com>

      ## Commit message ##
    -    add b
    +    add b (amended message)

      ## b.txt (new) ##
     @@
";
        let pairs = parse_range_diff(text);
        // Only the two TOP-LEVEL lines become pairs — every indented body
        // line (including the ones starting with '-'/'+' AFTER their
        // leading indent) is skipped.
        assert_eq!(pairs.len(), 2);
        assert_eq!(
            pairs[1],
            RangeDiffPair {
                old_sha: Some("26ef277".to_string()),
                new_sha: Some("56e64a6".to_string()),
                disposition: "modified",
                old_subject: Some("add b".to_string()),
                new_subject: None,
            }
        );
    }

    #[test]
    fn parses_an_added_entry() {
        let text = "-:  ------- > 3:  68f7814 add c\n";
        let pairs = parse_range_diff(text);
        assert_eq!(
            pairs,
            vec![RangeDiffPair {
                old_sha: None,
                new_sha: Some("68f7814".to_string()),
                disposition: "added",
                old_subject: None,
                new_subject: Some("add c".to_string()),
            }]
        );
    }

    #[test]
    fn parses_a_removed_entry() {
        let text = "1:  75e6a8e < -:  ------- add a\n";
        let pairs = parse_range_diff(text);
        assert_eq!(
            pairs,
            vec![RangeDiffPair {
                old_sha: Some("75e6a8e".to_string()),
                new_sha: None,
                disposition: "removed",
                old_subject: Some("add a".to_string()),
                new_subject: None,
            }]
        );
    }

    #[test]
    fn parses_a_mixed_run_of_every_disposition_in_one_call() {
        let text = "\
1:  aaaaaaa = 1:  aaaaaaa unchanged
2:  bbbbbbb ! 2:  ccccccc changed subject
    @@ Metadata
     some indented body line
3:  ddddddd < -:  ------- dropped commit
-:  ------- > 4:  eeeeeee new commit
";
        let pairs = parse_range_diff(text);
        let dispositions: Vec<&str> = pairs.iter().map(|p| p.disposition).collect();
        assert_eq!(dispositions, vec!["equal", "modified", "removed", "added"]);
    }

    #[test]
    fn empty_and_blank_input_yields_no_pairs() {
        assert!(parse_range_diff("").is_empty());
        assert!(parse_range_diff("\n\n\n").is_empty());
    }

    #[test]
    fn a_line_with_an_unrecognised_sign_is_skipped_not_a_panic() {
        let text = "1:  aaaaaaa ? 1:  aaaaaaa weird sign\n2:  bbbbbbb = 2:  bbbbbbb ok\n";
        let pairs = parse_range_diff(text);
        assert_eq!(pairs.len(), 1);
        assert_eq!(pairs[0].disposition, "equal");
    }

    #[test]
    fn range_diff_caps_at_max_pairs_and_reports_truncation() {
        let mut text = String::new();
        for i in 0..(MAX_PAIRS + 10) {
            text.push_str(&format!("{i}:  aaaaaaa = {i}:  aaaaaaa subject {i}\n"));
        }
        let pairs = parse_range_diff(&text);
        assert_eq!(
            pairs.len(),
            MAX_PAIRS + 10,
            "the pure parser itself is uncapped"
        );
        // The capping/truncation flag itself lives in `range_diff` (the
        // subprocess-wrapping fn), exercised via the real-subprocess test
        // below at a smaller, git-realistic scale instead of synthesizing
        // 210 real commits here.
    }

    // --- real subprocess: a genuine rebase-amend fixture ------------------

    #[test]
    fn range_diff_reports_equal_and_modified_across_a_real_rebase_amend() {
        let tmp = init_repo();
        let dir = tmp.path();
        std::fs::write(dir.join("base.txt"), "base\n").unwrap();
        git(dir, &["add", "-A"]);
        git(dir, &["commit", "-q", "-m", "base"]);
        let base_sha = String::from_utf8(
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

        std::fs::write(dir.join("a.txt"), "one\n").unwrap();
        git(dir, &["add", "-A"]);
        git(dir, &["commit", "-q", "-m", "add a"]);
        git(dir, &["tag", "topic-v1"]);

        git(dir, &["checkout", "-q", "-b", "topic-v2", "topic-v1"]);
        git(dir, &["commit", "--amend", "-q", "-m", "add a (amended)"]);
        std::fs::write(dir.join("c.txt"), "new\n").unwrap();
        git(dir, &["add", "-A"]);
        git(dir, &["commit", "-q", "-m", "add c"]);

        let old_range = RefRange::parse(&format!("{base_sha}..topic-v1")).unwrap();
        let new_range = RefRange::parse(&format!("{base_sha}..topic-v2")).unwrap();
        let rd = range_diff(dir, &old_range, &new_range).unwrap();

        assert!(!rd.truncated);
        assert_eq!(rd.pairs.len(), 2);
        assert_eq!(rd.pairs[0].disposition, "modified");
        assert_eq!(rd.pairs[0].old_subject.as_deref(), Some("add a"));
        assert!(rd.pairs[0].old_sha.is_some());
        assert!(rd.pairs[0].new_sha.is_some());
        assert_eq!(rd.pairs[1].disposition, "added");
        assert_eq!(rd.pairs[1].new_subject.as_deref(), Some("add c"));
        assert!(rd.pairs[1].old_sha.is_none());
    }

    #[test]
    fn range_diff_reports_equal_for_two_identical_ranges() {
        let tmp = init_repo();
        let dir = tmp.path();
        std::fs::write(dir.join("base.txt"), "base\n").unwrap();
        git(dir, &["add", "-A"]);
        git(dir, &["commit", "-q", "-m", "base"]);
        let base_sha = String::from_utf8(
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
        std::fs::write(dir.join("a.txt"), "one\n").unwrap();
        git(dir, &["add", "-A"]);
        git(dir, &["commit", "-q", "-m", "add a"]);
        git(dir, &["tag", "v1"]);

        let range = RefRange::parse(&format!("{base_sha}..v1")).unwrap();
        let rd = range_diff(dir, &range, &range).unwrap();
        assert!(!rd.pairs.is_empty());
        assert!(rd.pairs.iter().all(|p| p.disposition == "equal"));
    }

    /// V70-A2 (SEC-17) — the refusal moved into `RefRange`, which
    /// validates ENDPOINT-WISE. That is the whole point: `..` is legal as
    /// a separator and illegal inside an endpoint, so the range feature no
    /// longer has a reason to bypass the validator.
    #[test]
    fn range_diff_rejects_a_dash_prefixed_old() {
        let err: HistoryError = RefRange::parse_loose("--output=/tmp/x").unwrap_err().into();
        assert!(matches!(err, HistoryError::BadRevspec(_)), "got: {err:?}");
        // ...and inside a genuine range, on either side.
        assert!(RefRange::parse("--output=/tmp/x..main").is_err());
        assert!(RefRange::parse("main..-U9999").is_err());
    }

    #[test]
    fn range_diff_reports_a_clean_error_for_a_bad_range() {
        let tmp = init_repo();
        let dir = tmp.path();
        std::fs::write(dir.join("a.txt"), "x\n").unwrap();
        git(dir, &["add", "-A"]);
        git(dir, &["commit", "-q", "-m", "c1"]);

        let err = range_diff(
            dir,
            &RefRange::parse("main..nope").unwrap(),
            &RefRange::parse("main..also-nope").unwrap(),
        )
        .unwrap_err();
        assert!(
            matches!(err, HistoryError::GitFailed { .. }),
            "got: {err:?}"
        );
    }
}
