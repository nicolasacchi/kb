//! Local (no-daemon-needed) commit resolution — the join ladder's trailer
//! arm and the squash arm's body-wide scan both run entirely off this,
//! never touching the network. See [`resolve_local`]'s doc for how it
//! combines `kb_core::vcs::resolve_commit` (reused verbatim for the
//! OFFICIAL trailer-block parse, so a locally-detected `Kb-Session` trailer
//! is governed by the exact semantics kb's own `kb sessions capture`
//! already trusts) with this crate's gix handle (`crate::git::GitRepo`, via
//! `git::commit_info`) for the author TIMESTAMP `kb_core::vcs` doesn't
//! carry.

use crate::git::GitRepo;
use std::path::Path;

/// The `Kb-Session` trailer key, as stamped by `plugins/kb-memory/hooks/
/// git-dispatch/trailer-logic.sh` (`git interpret-trailers --trailer
/// "Kb-Session: $sid"`). Matched case-insensitively (git's own trailer
/// tooling doesn't force a canonical case, and this repo's only writer
/// happens to always use this exact casing, but a hand-edited message
/// shouldn't silently miss).
const TRAILER_KEY: &str = "kb-session";

/// One commit's LOCAL resolution — no daemon round trip. `resolved` and
/// every field through `trailers` come from `kb_core::vcs::resolve_commit`
/// (a `git show -s` shell-out); `author_time`/`raw_message` come from a
/// SEPARATE gix (in-process ODB) read via [`crate::git::GitRepo::commit_info`]
/// — see the module doc for why two reads instead of one.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct LocalCommit {
    /// `kb_core::vcs::ResolvedCommit::resolved` — `true` only when the
    /// `git show` resolution itself succeeded (repo found, sha resolvable).
    pub resolved: bool,
    /// Full 40/64-hex hash, when resolved. The join ladder's canonical
    /// cache key and the sha it passes to kb's `by-commit`/`commit-map`
    /// endpoints — see `join::ladder::resolve_commit`.
    pub sha_full: Option<String>,
    pub subject: Option<String>,
    /// The OFFICIAL trailer block (`%(trailers:only,unfold)` — git's own
    /// last-paragraph-only semantics). Empty when none were found, or when
    /// resolution failed. May legitimately contain MULTIPLE `Kb-Session:`
    /// lines (`trailer-logic.sh`'s "a commit can legitimately span more
    /// than one session" — `--amend` from a later session appends rather
    /// than replaces) — [`kb_session_trailer`] returns the first.
    pub trailers: Vec<String>,
    /// Author timestamp, unix seconds — `None` when gix couldn't resolve or
    /// read the commit (a missing local repo, an unresolvable sha, or a
    /// shallow-clone boundary). The repo-scoped time-window arm's only
    /// input; that arm simply can't run without this.
    pub author_time: Option<i64>,
    /// `title` + (`"\n\n"` + `body` when present) — the squash arm's
    /// whole-message scan source (see [`kb_session_body_scan`]).
    /// Empty when gix resolution failed.
    pub raw_message: String,
}

/// Combine `kb_core::vcs::resolve_commit` (subject/official-trailers/
/// sha_full — the exact semantics kb's own capture path trusts) with a gix
/// read for author-time + raw message. `sha` may be a short prefix; when
/// `kb_core::vcs` resolves it to a full sha, THAT is what's handed to gix
/// (so both halves agree on exactly which object they're describing even if
/// gix's own independent prefix resolution would — in a pathological
/// ambiguous-prefix repo — disagree with git's). Never errors: any failure
/// in either half degrades the corresponding fields to
/// `None`/empty/`false`, mirroring `kb_core::vcs::resolve_commit`'s own
/// "capture-time resolution never fails the caller" contract.
pub async fn resolve_local(repo_path: &Path, sha: &str) -> LocalCommit {
    let vcs = kb_core::vcs::resolve_commit(repo_path, sha).await;
    let spec = vcs.sha_full.as_deref().unwrap_or(sha);
    let (author_time, raw_message) =
        match GitRepo::open(repo_path).and_then(|g| g.commit_info(spec)) {
            Ok(info) => {
                let raw = match info.body {
                    Some(body) => format!("{}\n\n{body}", info.title),
                    None => info.title,
                };
                (Some(info.author_time_unix), raw)
            }
            Err(_) => (None, String::new()),
        };
    LocalCommit {
        resolved: vcs.resolved,
        sha_full: vcs.sha_full,
        subject: vcs.subject,
        trailers: vcs.trailers,
        author_time,
        raw_message,
    }
}

/// The FIRST `Kb-Session:` trailer's value from the OFFICIAL trailer block
/// (`LocalCommit::trailers`, git's own last-paragraph-only parse) — the
/// join ladder's trailer arm. Case-insensitive key match; the value is
/// trimmed and rejected if empty (a malformed/hand-edited `Kb-Session:`
/// with nothing after the colon is not a usable session id).
pub fn kb_session_trailer(trailers: &[String]) -> Option<String> {
    trailers.iter().find_map(|line| trailer_value(line))
}

/// Every `Kb-Session:` occurrence found by scanning `raw_message` LINE BY
/// LINE, not restricted to git's official trailing-paragraph block — the
/// squash arm's job: a squashed/merged commit's body often concatenates
/// several original commits' full messages (each carrying its own trailer
/// paragraph buried mid-body), which [`kb_session_trailer`]'s last-
/// paragraph-only parse would never see. Returns the FIRST match,
/// deterministic top-to-bottom (same tie-break convention as
/// [`kb_session_trailer`]).
pub fn kb_session_body_scan(raw_message: &str) -> Option<String> {
    raw_message.lines().find_map(trailer_value)
}

/// `"Kb-Session: <value>"` (case-insensitive key, arbitrary surrounding
/// whitespace) → `Some(<trimmed value>)`, or `None` for any other line
/// (including a bare `"Kb-Session:"` with nothing after the colon).
fn trailer_value(line: &str) -> Option<String> {
    let (key, value) = line.split_once(':')?;
    if !key.trim().eq_ignore_ascii_case(TRAILER_KEY) {
        return None;
    }
    let value = value.trim();
    (!value.is_empty()).then(|| value.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::Command;

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

    fn git_out(dir: &Path, args: &[&str]) -> String {
        let out = Command::new("git")
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

    fn commit_with_message(
        dir: &Path,
        file: &str,
        contents: &str,
        message: &str,
        author_date: &str,
    ) {
        std::fs::write(dir.join(file), contents).unwrap();
        git(dir, &["add", file]);
        let status = Command::new("git")
            .arg("-C")
            .arg(dir)
            .args(["commit", "-q", "-m", message])
            .env("GIT_AUTHOR_DATE", author_date)
            .env("GIT_COMMITTER_DATE", author_date)
            .status()
            .expect("git commit runs");
        assert!(status.success());
    }

    #[tokio::test]
    async fn resolve_local_reads_subject_trailer_and_author_time() {
        let tmp = init_repo();
        let dir = tmp.path();
        commit_with_message(
            dir,
            "a.txt",
            "hello\n",
            "the subject\n\nKb-Session: sess-abc\n",
            "1700000000 +0000",
        );
        let sha = git_out(dir, &["rev-parse", "HEAD"]);

        let local = resolve_local(dir, &sha).await;
        assert!(local.resolved);
        assert_eq!(local.sha_full.as_deref(), Some(sha.as_str()));
        assert_eq!(local.subject.as_deref(), Some("the subject"));
        assert_eq!(local.trailers, vec!["Kb-Session: sess-abc".to_string()]);
        assert_eq!(local.author_time, Some(1_700_000_000));
        assert!(local.raw_message.contains("the subject"));
        assert!(local.raw_message.contains("Kb-Session: sess-abc"));
    }

    #[tokio::test]
    async fn resolve_local_disambiguates_a_short_prefix() {
        let tmp = init_repo();
        let dir = tmp.path();
        commit_with_message(dir, "a.txt", "hello\n", "c1", "1700000000 +0000");
        let sha = git_out(dir, &["rev-parse", "HEAD"]);
        let short = &sha[..8];

        let local = resolve_local(dir, short).await;
        assert!(local.resolved);
        assert_eq!(local.sha_full.as_deref(), Some(sha.as_str()));
    }

    #[tokio::test]
    async fn resolve_local_degrades_cleanly_on_a_non_repo_path() {
        let tmp = tempfile::tempdir().unwrap();
        let local = resolve_local(tmp.path(), "deadbeef").await;
        assert!(!local.resolved);
        assert_eq!(local.sha_full, None);
        assert_eq!(local.author_time, None);
        assert_eq!(local.raw_message, "");
    }

    #[tokio::test]
    async fn resolve_local_degrades_cleanly_on_an_unresolvable_sha() {
        let tmp = init_repo();
        let dir = tmp.path();
        commit_with_message(dir, "a.txt", "hello\n", "c1", "1700000000 +0000");

        let local = resolve_local(dir, "deadbeefdeadbeef").await;
        assert!(!local.resolved);
        assert_eq!(local.author_time, None);
        assert_eq!(local.raw_message, "");
    }

    #[test]
    fn kb_session_trailer_reads_the_first_line_case_insensitively() {
        let trailers = vec![
            "Signed-off-by: someone <s@example.com>".to_string(),
            "kb-SESSION: sess-1".to_string(),
            "Kb-Session: sess-2".to_string(),
        ];
        assert_eq!(kb_session_trailer(&trailers).as_deref(), Some("sess-1"));
    }

    #[test]
    fn kb_session_trailer_rejects_an_empty_value_and_a_missing_key() {
        assert_eq!(kb_session_trailer(&["Kb-Session:".to_string()]), None);
        assert_eq!(kb_session_trailer(&["Kb-Session:   ".to_string()]), None);
        assert_eq!(
            kb_session_trailer(&["Not-Kb-Session: sess-1".to_string()]),
            None
        );
        assert_eq!(kb_session_trailer(&[]), None);
    }

    #[test]
    fn kb_session_body_scan_finds_a_trailer_buried_mid_body() {
        let raw = "Squashed commit of the following:\n\n\
                    commit abc123\n\
                    Author: someone\n\n    fix the thing\n\n    Kb-Session: sess-buried\n\n\
                    commit def456\n\
                    Author: someone else\n\n    another commit\n";
        assert_eq!(
            kb_session_body_scan(raw).as_deref(),
            Some("sess-buried"),
            "a strict last-paragraph trailer parse would miss this — the whole-body scan must not"
        );
    }

    #[test]
    fn kb_session_body_scan_returns_none_when_absent() {
        assert_eq!(kb_session_body_scan("just a subject\n\nand a body"), None);
    }
}
