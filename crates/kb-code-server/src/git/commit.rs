//! Commit metadata reads (W3.2, the join ladder) — a small, focused
//! extension of this module's read-only gix wrapper. `kb_core::vcs::
//! resolve_commit` (the SAME `git show -s` shell-out kb's own `kb sessions
//! capture` uses to populate `session_commits.{subject,sha_full,repo_root,
//! parents,trailers}`, V0025) is deliberately reused elsewhere
//! (`crate::join::local`) for those fields — reusing it byte-for-byte
//! guarantees a locally-detected `Kb-Session` trailer is governed by the
//! EXACT same trailer-block parse (`%(trailers:only,unfold)`) the daemon
//! already trusts, rather than a second, possibly-drifting implementation.
//!
//! What `kb_core::vcs::ResolvedCommit` does NOT carry is the commit's
//! AUTHOR TIMESTAMP — kb never stores it (only the `"Name <email>"` identity
//! string, `%an <%ae>`), because no daemon-side feature has needed it. The
//! join ladder's repo-scoped time-window arm does: "does this session's
//! capture window cover the commit's author-time." Extending `kb_core::vcs`
//! to also capture `%at` would ripple into kb's `session_commits` schema
//! (V0025) and every downstream consumer (`CommitOut`, the capture
//! envelope) — well outside a kb-code-only join module's blast radius. So
//! this crate's OWN existing gix handle (`GitRepo`, W1.3) reads it directly
//! instead: `commit.author()?.time.seconds`, one in-process ODB lookup, no
//! subprocess.
//!
//! While here, this also hands back the reconstructed title+body TEXT
//! (`title` / `body`) — the squash arm's whole-message `Kb-Session:` scan
//! source (`crate::join::ladder`'s squash-trailer sub-arm), deliberately
//! NOT run through any trailer-block parser: a squashed merge commit's body
//! often concatenates several original commits' FULL messages (each with
//! its own trailer paragraph buried mid-body), which a last-paragraph-only
//! parse (this module's own `kb_core::vcs`-backed trailer arm) would miss
//! by design.

use super::{GitError, GitRepo};

/// One commit's gix-read metadata — see the module doc for why this exists
/// alongside (not instead of) `kb_core::vcs::resolve_commit`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommitInfo {
    /// Full hex object id, as resolved by [`GitRepo::resolve`] — the SAME
    /// disambiguation (prefix → full sha, shallow-clone-aware) every other
    /// `GitRepo` method uses.
    pub sha: String,
    /// Author timestamp, unix seconds (`commit.author()?.time.seconds` —
    /// NOT the committer time `gix::Commit::time()` would give; a commit's
    /// AUTHOR time is what the join ladder's window arm needs, since that's
    /// when the work was actually done, not when it was last rewritten).
    pub author_time_unix: i64,
    /// First line of the commit message (`MessageRef::title`).
    pub title: String,
    /// Everything after the title's blank-line separator
    /// (`MessageRef::body`), verbatim — `None` for a title-only message.
    pub body: Option<String>,
    /// `commit.parent_ids().count()` — 0 for a root commit, 2+ for a merge.
    pub parent_count: usize,
}

/// Resolve `rev` (any revspec `GitRepo::resolve` accepts — full/short sha,
/// branch, tag, `HEAD~n`, ...) and read its author time + message + parent
/// count via gix, in-process. Errors mirror `GitRepo`'s own convention
/// (`GitError::Resolve` for an unresolvable revspec, `GitError::Odb` for an
/// object-database read failure past that point) — the join ladder's own
/// caller (`crate::join::local::resolve_local`) treats ANY error here as
/// "no author-time / no raw message available," never a hard failure (see
/// that function's doc).
pub fn commit_info(git: &GitRepo, rev: &str) -> Result<CommitInfo, GitError> {
    let oid = git.resolve(rev)?;
    let commit = git.repo.find_commit(oid).map_err(|e| GitError::Odb {
        message: format!("read commit {oid}: {e}"),
    })?;
    let author = commit.author().map_err(|e| GitError::Odb {
        message: format!("read commit {oid} author: {e}"),
    })?;
    // `SignatureRef::time` is the RAW, unparsed date string (round-trip
    // fidelity — see gix-actor's own doc); `.time()` (the method, not the
    // field) decodes it into `gix_date::Time { seconds, offset }`.
    let author_time = author.time().map_err(|e| GitError::Odb {
        message: format!("read commit {oid} author time: {e}"),
    })?;
    let message = commit.message().map_err(|e| GitError::Odb {
        message: format!("read commit {oid} message: {e}"),
    })?;
    let parent_count = commit.parent_ids().count();
    Ok(CommitInfo {
        sha: oid.to_string(),
        author_time_unix: author_time.seconds,
        title: message.title.to_string(),
        body: message.body.map(|b| b.to_string()),
        parent_count,
    })
}

impl GitRepo {
    /// See [`commit_info`] (free fn, mirrors this module's own
    /// `refs::list_refs(self)` / `tree::list_tree(self, ...)` delegation
    /// convention — `GitRepo`'s inherent methods are thin wrappers over a
    /// per-concern submodule fn).
    pub fn commit_info(&self, rev: &str) -> Result<CommitInfo, GitError> {
        commit_info(self, rev)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;
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

    /// One commit, with a controlled author DATE (`GIT_AUTHOR_DATE`) so the
    /// test can assert the exact unix timestamp gix reads back, and a
    /// two-paragraph body (subject + a blank line + a body paragraph,
    /// itself containing a `Trailer-Key: value` line) so title/body
    /// splitting is exercised.
    fn repo_with_one_commit() -> (tempfile::TempDir, String) {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path();
        git(dir, &["init", "-q", "-b", "main"]);
        git(dir, &["config", "user.email", "test@example.com"]);
        git(dir, &["config", "user.name", "Test"]);
        std::fs::write(dir.join("a.txt"), b"hello\n").unwrap();
        git(dir, &["add", "a.txt"]);
        let msg = "the subject line\n\nbody paragraph one\n\nTrailer-Key: trailer-value\n";
        let status = Command::new("git")
            .arg("-C")
            .arg(dir)
            .args(["commit", "-q", "-m", msg])
            .env("GIT_AUTHOR_DATE", "1700000000 +0000")
            .env("GIT_COMMITTER_DATE", "1700000500 +0000")
            .status()
            .expect("git commit runs");
        assert!(status.success());
        let sha = git_out(dir, &["rev-parse", "HEAD"]);
        (tmp, sha)
    }

    #[test]
    fn commit_info_reads_author_time_title_body_and_parent_count() {
        let (tmp, sha) = repo_with_one_commit();
        let repo = GitRepo::open(tmp.path()).unwrap();
        let info = repo.commit_info("HEAD").unwrap();
        assert_eq!(info.sha, sha);
        assert_eq!(info.author_time_unix, 1_700_000_000);
        assert_eq!(info.title, "the subject line");
        // gix preserves the body's own trailing newline verbatim (lossless
        // round-tripping is the whole point of `SignatureRef`/`MessageRef`
        // borrowing straight off the raw object bytes) — not stripped here.
        assert_eq!(
            info.body.as_deref(),
            Some("body paragraph one\n\nTrailer-Key: trailer-value\n")
        );
        assert_eq!(info.parent_count, 0);
    }

    #[test]
    fn commit_info_resolves_a_short_prefix_the_same_as_the_full_sha() {
        let (tmp, sha) = repo_with_one_commit();
        let repo = GitRepo::open(tmp.path()).unwrap();
        let short = &sha[..8];
        let by_prefix = repo.commit_info(short).unwrap();
        let by_full = repo.commit_info(&sha).unwrap();
        assert_eq!(by_prefix, by_full);
    }

    #[test]
    fn commit_info_counts_two_parents_on_a_merge_commit() {
        let (tmp, _sha) = repo_with_one_commit();
        let dir = tmp.path();
        git(dir, &["branch", "feature"]);
        git(dir, &["checkout", "-q", "feature"]);
        std::fs::write(dir.join("b.txt"), b"feature\n").unwrap();
        git(dir, &["add", "b.txt"]);
        git(dir, &["commit", "-q", "-m", "feature commit"]);
        git(dir, &["checkout", "-q", "main"]);
        std::fs::write(dir.join("c.txt"), b"main\n").unwrap();
        git(dir, &["add", "c.txt"]);
        git(dir, &["commit", "-q", "-m", "main commit"]);
        git(
            dir,
            &["merge", "-q", "--no-ff", "feature", "-m", "merge it"],
        );

        let repo = GitRepo::open(dir).unwrap();
        let info = repo.commit_info("HEAD").unwrap();
        assert_eq!(info.parent_count, 2);
        // A single-line message has no blank-line body separator at all, so
        // `title` is the WHOLE message verbatim, trailing newline included
        // (same lossless-round-trip behaviour as the body case above).
        assert_eq!(info.title, "merge it\n");
        assert_eq!(info.body, None);
    }

    #[test]
    fn commit_info_on_an_unresolvable_rev_errors_cleanly() {
        let (tmp, _sha) = repo_with_one_commit();
        let repo = GitRepo::open(tmp.path()).unwrap();
        let err = repo.commit_info("deadbeef00").unwrap_err();
        assert!(matches!(err, GitError::Resolve { .. }), "got: {err:?}");
    }
}
