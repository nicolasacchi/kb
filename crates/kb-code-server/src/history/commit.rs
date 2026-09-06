//! `GET /api/commit` — the commit page hub (Phase C1's server half): one
//! commit's full metadata + numstat/name-status file list. (Attribution
//! through the join ladder is layered on by `routes::commit_route` itself
//! — this module has no `join`/`store` dependency, matching `crate::diff`'s
//! own "narrow, directly-testable" precedent.)
//!
//! # gix vs. subprocess — metadata
//!
//! Metadata comes from ONE `git show -s --format=...` subprocess call —
//! the SAME `%(trailers:only,unfold)` placeholder `kb_core::vcs::
//! resolve_commit`/`join::local` already trust (git's own official,
//! last-paragraph-only trailer-block parse), plus separate author/
//! committer name+email+timestamp and the full parent sha list — none of
//! which `crate::git::commit_info` (gix) exposes today (that fn is scoped
//! to exactly what the join ladder needs: author time, title/body, parent
//! COUNT — see its own module doc). Subprocess, not gix, was the right
//! call here specifically because ONE `git show -s --format=` round trip
//! gets EVERY one of these fields at once; doing the same over gix would
//! mean hand-parsing the raw commit object AND reimplementing git's own
//! trailer-block last-paragraph rule (gix has no ready-made equivalent to
//! `%(trailers:only,unfold)`) for no benefit — strictly more code for a
//! result `git show` already produces in one call.
//!
//! `%H` in the format string is what disambiguates a short sha prefix to
//! the full 40/64-hex hash — the exact value [`commit_meta`]'s caller
//! (`routes::commit_route`) then feeds to the join ladder and to
//! [`commit_files`] below, so every downstream read agrees on which
//! object it's describing.
//!
//! # File list
//!
//! `git diff-tree --no-commit-id -r --root -M <sha>`, numstat + name-status
//! merged via `super::diff_files`. `--root` makes diff-tree diff a ROOT
//! commit (no parent) against the empty tree directly — no manual
//! empty-tree-hash trick needed (verified against a real root commit in
//! this module's own tests); it is a no-op for every other commit.

use super::{diff_files, HistoryError, Person, Result, Trailer};
use crate::numstat::FileChange;
use std::path::Path;
use std::process::Command;

/// `%H` (full sha) `%x1f` `%an` `%x1f` `%ae` `%x1f` `%at` `%x1f` `%cn`
/// `%x1f` `%ce` `%x1f` `%ct` `%x1f` `%P` (parents) `%x1e` `%s` (subject)
/// `%x1e` `%b` (body) `%x1e` `%(trailers:only,unfold)`. Two separators
/// (`%x1f` unit / `%x1e` record) so a subject/author name containing a
/// literal tab or similar never confuses the split — mirrors
/// `kb_core::vcs::RESOLVE_FMT`'s own two-tier separator convention.
const COMMIT_META_FMT: &str =
    "%H%x1f%an%x1f%ae%x1f%at%x1f%cn%x1f%ce%x1f%ct%x1f%P%x1e%s%x1e%b%x1e%(trailers:only,unfold)";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommitMeta {
    /// Full hex sha — see the module doc.
    pub sha: String,
    pub subject: String,
    /// `None` for a subject-only message (git's `%b` is the empty string
    /// in that case) — mirrors `crate::git::commit::CommitInfo::body`'s
    /// own `None`-means-title-only convention.
    pub body: Option<String>,
    pub author: Person,
    pub committer: Person,
    /// Full hex shas, in `git show`'s own order — empty for a root commit.
    pub parents: Vec<String>,
    pub trailers: Vec<Trailer>,
}

/// `"Key: value"` lines (git's official trailer-block text) → structured
/// `Trailer`s. A line without a `:`, or with an empty key/value, is
/// skipped defensively (should not occur in practice — the block is
/// git's own `%(trailers:only,unfold)` output — but this never panics on
/// one either way).
fn parse_trailers(block: &str) -> Vec<Trailer> {
    block
        .lines()
        .filter_map(|line| {
            let (key, value) = line.split_once(':')?;
            let key = key.trim();
            let value = value.trim();
            if key.is_empty() || value.is_empty() {
                return None;
            }
            Some(Trailer {
                key: key.to_string(),
                value: value.to_string(),
            })
        })
        .collect()
}

fn parse_commit_meta(stdout: &str) -> Option<CommitMeta> {
    let mut sections = stdout.splitn(4, '\u{1e}');
    let head = sections.next()?;
    let subject = sections.next()?.to_string();
    let body_raw = sections.next()?;
    let trailers_block = sections.next().unwrap_or("");

    let mut f = head.split('\u{1f}');
    let sha = f.next()?.to_string();
    let author_name = f.next()?.to_string();
    let author_email = f.next()?.to_string();
    let author_time: i64 = f.next()?.parse().ok()?;
    let committer_name = f.next()?.to_string();
    let committer_email = f.next()?.to_string();
    let committer_time: i64 = f.next()?.parse().ok()?;
    let parents_raw = f.next()?;

    let parents: Vec<String> = parents_raw.split_whitespace().map(str::to_string).collect();
    let body = if body_raw.is_empty() {
        None
    } else {
        Some(body_raw.to_string())
    };

    Some(CommitMeta {
        sha,
        subject,
        body,
        author: Person {
            name: author_name,
            email: author_email,
            time: author_time,
        },
        committer: Person {
            name: committer_name,
            email: committer_email,
            time: committer_time,
        },
        parents,
        trailers: parse_trailers(trailers_block),
    })
}

/// `git show -s --format=<COMMIT_META_FMT> <sha>` — see the module doc.
/// [`HistoryError::NotFound`] (NOT `GitFailed`) on a non-zero exit — a
/// well-formed-but-unresolvable sha is the near-exclusive real-world cause
/// (`sha` already passed `join::ladder::is_plausible_sha`'s shape gate
/// before reaching here, per `routes::commit_route`).
pub fn commit_meta(repo_root: &Path, sha: &str) -> Result<CommitMeta> {
    let output = Command::new("git")
        .arg("-C")
        .arg(repo_root)
        .args(["show", "-s", &format!("--format={COMMIT_META_FMT}")])
        .arg(sha)
        .output()
        .map_err(HistoryError::Spawn)?;
    if !output.status.success() {
        return Err(HistoryError::NotFound(sha.to_string()));
    }
    let stdout = String::from_utf8_lossy(&output.stdout);
    parse_commit_meta(&stdout).ok_or_else(|| HistoryError::NotFound(sha.to_string()))
}

/// `git diff-tree --no-commit-id -r --root -M <sha>` numstat+name-status,
/// merged — see the module doc's "File list" section. `sha` should be the
/// FULL sha [`commit_meta`] already resolved (not a possibly-short caller
/// prefix), so this and `commit_meta` never disagree about which commit
/// they're describing.
pub fn commit_files(repo_root: &Path, sha: &str) -> Result<Vec<FileChange>> {
    diff_files(
        repo_root,
        "diff-tree",
        &["--no-commit-id", "-r", "--root", "-M", sha],
    )
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
    fn parse_commit_meta_splits_every_field_including_trailers() {
        let stdout = "sha1\u{1f}An Author\u{1f}a@example.com\u{1f}1700000000\u{1f}A Committer\u{1f}c@example.com\u{1f}1700000500\u{1f}parent1 parent2\u{1e}the subject\u{1e}body para\n\nKb-Session: sess-1\n\u{1e}Kb-Session: sess-1\n";
        let meta = parse_commit_meta(stdout).unwrap();
        assert_eq!(meta.sha, "sha1");
        assert_eq!(meta.subject, "the subject");
        assert_eq!(
            meta.body.as_deref(),
            Some("body para\n\nKb-Session: sess-1\n")
        );
        assert_eq!(meta.author.name, "An Author");
        assert_eq!(meta.author.email, "a@example.com");
        assert_eq!(meta.author.time, 1_700_000_000);
        assert_eq!(meta.committer.name, "A Committer");
        assert_eq!(meta.committer.time, 1_700_000_500);
        assert_eq!(meta.parents, vec!["parent1", "parent2"]);
        assert_eq!(
            meta.trailers,
            vec![Trailer {
                key: "Kb-Session".to_string(),
                value: "sess-1".to_string()
            }]
        );
    }

    #[test]
    fn parse_commit_meta_treats_an_empty_body_as_none() {
        let stdout = "sha1\u{1f}A\u{1f}a@x\u{1f}1\u{1f}A\u{1f}a@x\u{1f}1\u{1f}\u{1e}subject only\u{1e}\u{1e}\n";
        let meta = parse_commit_meta(stdout).unwrap();
        assert_eq!(meta.body, None);
        assert_eq!(meta.parents, Vec::<String>::new());
    }

    #[test]
    fn commit_meta_reads_real_author_committer_parents_and_trailer() {
        let tmp = init_repo();
        let dir = tmp.path();
        std::fs::write(dir.join("a.txt"), "hello\n").unwrap();
        git(dir, &["add", "a.txt"]);
        let msg = "the subject\n\nbody para\n\nKb-Session: sess-abc\n";
        let status = StdCommand::new("git")
            .arg("-C")
            .arg(dir)
            .args(["commit", "-q", "-m", msg])
            .env("GIT_AUTHOR_DATE", "1700000000 +0000")
            .env("GIT_COMMITTER_DATE", "1700000500 +0000")
            .status()
            .unwrap();
        assert!(status.success());
        let sha = git_out(dir, &["rev-parse", "HEAD"]);

        let meta = commit_meta(dir, &sha[..8]).unwrap();
        assert_eq!(meta.sha, sha, "a short prefix must resolve to the FULL sha");
        assert_eq!(meta.subject, "the subject");
        assert_eq!(meta.author.name, "Test");
        assert_eq!(meta.author.email, "test@example.com");
        assert_eq!(meta.author.time, 1_700_000_000);
        assert_eq!(meta.committer.time, 1_700_000_500);
        assert_eq!(
            meta.parents,
            Vec::<String>::new(),
            "root commit has no parents"
        );
        assert_eq!(
            meta.trailers,
            vec![Trailer {
                key: "Kb-Session".to_string(),
                value: "sess-abc".to_string()
            }]
        );
    }

    #[test]
    fn commit_meta_reports_two_parents_for_a_merge() {
        let tmp = init_repo();
        let dir = tmp.path();
        std::fs::write(dir.join("a.txt"), "one\n").unwrap();
        git(dir, &["add", "a.txt"]);
        git(dir, &["commit", "-q", "-m", "base"]);
        git(dir, &["branch", "feature"]);
        git(dir, &["checkout", "-q", "feature"]);
        std::fs::write(dir.join("b.txt"), "feature\n").unwrap();
        git(dir, &["add", "b.txt"]);
        git(dir, &["commit", "-q", "-m", "feature commit"]);
        git(dir, &["checkout", "-q", "main"]);
        std::fs::write(dir.join("c.txt"), "main\n").unwrap();
        git(dir, &["add", "c.txt"]);
        git(dir, &["commit", "-q", "-m", "main commit"]);
        git(
            dir,
            &["merge", "-q", "--no-ff", "feature", "-m", "merge it"],
        );
        let sha = git_out(dir, &["rev-parse", "HEAD"]);

        let meta = commit_meta(dir, &sha).unwrap();
        assert_eq!(meta.parents.len(), 2);
    }

    #[test]
    fn commit_meta_on_an_unresolvable_sha_is_not_found() {
        let tmp = init_repo();
        let dir = tmp.path();
        std::fs::write(dir.join("a.txt"), "one\n").unwrap();
        git(dir, &["add", "a.txt"]);
        git(dir, &["commit", "-q", "-m", "c1"]);

        let err = commit_meta(dir, "deadbeefdeadbeefdead").unwrap_err();
        assert!(matches!(err, HistoryError::NotFound(_)), "got: {err:?}");
    }

    #[test]
    fn commit_files_reports_a_root_commits_files_via_the_root_flag() {
        let tmp = init_repo();
        let dir = tmp.path();
        std::fs::write(dir.join("a.txt"), "line1\nline2\n").unwrap();
        git(dir, &["add", "a.txt"]);
        git(dir, &["commit", "-q", "-m", "root"]);
        let sha = git_out(dir, &["rev-parse", "HEAD"]);

        let files = commit_files(dir, &sha).unwrap();
        assert_eq!(files.len(), 1);
        assert_eq!(files[0].path, "a.txt");
        assert_eq!(files[0].status, "A");
        assert_eq!(files[0].insertions, 2);
        assert_eq!(files[0].deletions, 0);
        assert!(!files[0].binary);
    }

    #[test]
    fn commit_files_reports_a_rename_with_the_old_path() {
        let tmp = init_repo();
        let dir = tmp.path();
        std::fs::write(dir.join("a.txt"), "hello\n").unwrap();
        git(dir, &["add", "a.txt"]);
        git(dir, &["commit", "-q", "-m", "c1"]);
        git(dir, &["mv", "a.txt", "renamed.txt"]);
        std::fs::write(dir.join("renamed.txt"), "hello\nmore\n").unwrap();
        git(dir, &["add", "-A"]);
        git(dir, &["commit", "-q", "-m", "rename it"]);
        let sha = git_out(dir, &["rev-parse", "HEAD"]);

        let files = commit_files(dir, &sha).unwrap();
        assert_eq!(files.len(), 1);
        assert_eq!(files[0].path, "renamed.txt");
        assert_eq!(files[0].old_path.as_deref(), Some("a.txt"));
        assert_eq!(files[0].status, "R");
    }

    #[test]
    fn commit_files_flags_a_binary_file_without_line_counts() {
        let tmp = init_repo();
        let dir = tmp.path();
        std::fs::write(dir.join("bin.dat"), [0u8, 1, 2, 0, 255]).unwrap();
        git(dir, &["add", "bin.dat"]);
        git(dir, &["commit", "-q", "-m", "add binary"]);
        let sha = git_out(dir, &["rev-parse", "HEAD"]);

        let files = commit_files(dir, &sha).unwrap();
        assert_eq!(files.len(), 1);
        assert!(files[0].binary);
        assert_eq!(files[0].insertions, 0);
        assert_eq!(files[0].deletions, 0);
    }
}
