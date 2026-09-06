//! `GET /api/merge-check` (Phase G-server — the review-workflow endpoints)
//! — dry-run merge readiness: `git merge-tree --write-tree --name-only
//! <from> <to>` (a WORKING-TREE-SAFE merge simulation — unlike `git
//! merge`, this invocation never touches the index or the working tree,
//! writing its would-be result tree straight into the object database and
//! nothing else) plus ahead/behind counts, reusing
//! [`super::branches::ahead_behind`] (`from...to`'s own left-right count is
//! EXACTLY "how far `to` is ahead/behind `from`", the same semantics that
//! fn already computes for `history::branches_route`'s default-vs-branch
//! case — no second rev-list wrapper needed).
//!
//! # Distinguishing a real conflict from an unresolvable revspec
//!
//! `git merge-tree --write-tree` exits **1** for BOTH a genuine conflict
//! AND an unresolvable ref (e.g. a typo'd branch name) — the exit code
//! alone can't tell them apart (verified against git 2.55: `git merge-tree
//! --write-tree --name-only main nope-branch` also exits 1). The two ARE
//! distinguishable by where the output lands: a genuine merge attempt
//! (clean OR conflicted) always writes its result tree's OID as stdout's
//! FIRST line, with stderr empty; an unresolvable ref writes NOTHING to
//! stdout and an error message to stderr instead. [`parse_merge_tree`]
//! below keys on stdout's first line looking like a plausible object id to
//! route a resolve failure to [`HistoryError::GitFailed`] (→ 400, a caller
//! mistake) rather than mis-reporting it as "clean" or "conflicted."
//!
//! # Output shape (a real conflict, exit 1)
//!
//! ```text
//! <tree oid>
//! <conflicted path>
//! <conflicted path>
//! ...
//! <blank line>
//! <informational messages...>
//! ```
//!
//! `--name-only` keeps the conflicted-file section to a bare, one-path-
//! per-line list (no per-stage blob shas) — porcelain enough to parse with
//! "read lines after line 1 until the first blank line." This call never
//! writes conflict markers into any file — the module's own "never
//! touches the working tree" guarantee holds regardless of the outcome.

use super::{merge_base, resolve_sha, HistoryError, Resolved, Result};
use crate::git::{RefRange, Revspec};
use crate::history::branches::ahead_behind;
use crate::history::scratch::ScratchOdb;
use std::path::Path;
use std::process::Command;

/// One conflicted path — a bare wrapper (not a plain `String`) so the wire
/// shape can grow per-path detail later (stage blobs, conflict kind, ...)
/// without a breaking change to `merge-check/1`.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct ConflictEntry {
    pub path: String,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct MergeCheck {
    pub resolved: Resolved,
    pub clean: bool,
    pub conflicts: Vec<ConflictEntry>,
    pub ahead: u32,
    pub behind: u32,
}

/// The raw `git merge-tree --write-tree --name-only <from> <to>` call —
/// kept separate from [`run_git_raw`](super::run_git_raw) on purpose: that
/// shared helper treats ANY non-zero exit as [`HistoryError::GitFailed`],
/// which would misclassify a genuine conflict (exit 1 is this command's
/// ordinary "conflicts found" signal, not a subprocess failure) — see the
/// module doc.
fn run_merge_tree(
    repo_root: &Path,
    scratch_root: &Path,
    from: &Revspec,
    to: &Revspec,
) -> Result<(i32, Vec<u8>, Vec<u8>)> {
    let scratch = ScratchOdb::create(repo_root, scratch_root)?;
    let output = Command::new("git")
        .arg("-C")
        .arg(repo_root)
        // SEC-15 — write the would-be result tree into a PER-REQUEST
        // scratch object directory instead of the browsed repo's own ODB.
        // Both vars are load-bearing and neither works alone:
        // `GIT_OBJECT_DIRECTORY` redirects WRITES but also makes the real
        // objects unreadable, so `GIT_ALTERNATE_OBJECT_DIRECTORIES` has to
        // point back at them for the merge to have anything to merge.
        .env("GIT_OBJECT_DIRECTORY", scratch.dir())
        .env("GIT_ALTERNATE_OBJECT_DIRECTORIES", scratch.alternates())
        .args([
            "merge-tree",
            "--write-tree",
            "--name-only",
            from.as_str(),
            to.as_str(),
        ])
        .output()
        .map_err(HistoryError::Spawn)?;
    Ok((
        output.status.code().unwrap_or(-1),
        output.stdout,
        output.stderr,
    ))
    // `scratch` drops here — see `ScratchOdb`'s Drop impl.
}

/// A line that looks like a plausible git object id (hex, non-empty) —
/// used here to classify git's OWN stdout, not to validate caller input
/// (unlike `join::ladder::is_plausible_sha`, which gates a caller-supplied
/// string and additionally bounds the length to 4-64; git's abbreviated or
/// full object ids are always well inside that range anyway).
fn looks_like_oid(line: &str) -> bool {
    let l = line.trim();
    !l.is_empty() && l.len() <= 64 && l.bytes().all(|b| b.is_ascii_hexdigit())
}

/// Parse `run_merge_tree`'s `(exit status, stdout, stderr)` triple into
/// `(clean, conflicted_paths)` — see the module doc for the exit-1-is-
/// ambiguous rationale. Split out from [`merge_check`] so the parser is
/// directly unit-testable against captured fixture bytes, no subprocess
/// required.
fn parse_merge_tree(status: i32, stdout: &[u8], stderr: &[u8]) -> Result<(bool, Vec<String>)> {
    let text = String::from_utf8_lossy(stdout);
    let mut lines = text.lines();
    let first_line = lines.next().unwrap_or("");

    match status {
        0 if looks_like_oid(first_line) => Ok((true, Vec::new())),
        1 if looks_like_oid(first_line) => {
            let conflicts: Vec<String> = lines
                .take_while(|l| !l.trim().is_empty())
                .map(|s| s.to_string())
                .collect();
            Ok((false, conflicts))
        }
        _ => Err(HistoryError::GitFailed {
            status,
            stderr: String::from_utf8_lossy(stderr).trim().to_string(),
        }),
    }
}

/// `GET /api/merge-check`'s business logic — see the module doc. `from`/
/// `to` get the same dash-prefix injection guard every other caller-
/// supplied revspec in this crate does.
pub fn merge_check(
    repo_root: &Path,
    scratch_root: &Path,
    from: &Revspec,
    to: &Revspec,
) -> Result<MergeCheck> {
    // V70-A2 (SEC-17) — `from`/`to` arrive already validated; the two
    // `reject_dash_prefixed` calls that used to live here are subsumed by
    // `Revspec`'s constructor, which is strictly stronger.
    let from_sha = resolve_sha(repo_root, from.as_str())?;
    let to_sha = resolve_sha(repo_root, to.as_str())?;
    let base = merge_base(repo_root, &from_sha, &to_sha);

    let (status, stdout, stderr) = run_merge_tree(repo_root, scratch_root, from, to)?;
    let (clean, conflict_paths) = parse_merge_tree(status, &stdout, &stderr)?;
    let conflicts = conflict_paths
        .into_iter()
        .map(|path| ConflictEntry { path })
        .collect();

    let ab = ahead_behind(repo_root, &RefRange::new(from.clone(), to.clone(), true))?;

    Ok(MergeCheck {
        resolved: Resolved {
            from_sha,
            to_sha,
            merge_base: base,
        },
        clean,
        conflicts,
        ahead: ab.ahead,
        behind: ab.behind,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::Command as StdCommand;

    fn rs(s: &str) -> Revspec {
        Revspec::parse(s).expect("fixture revspec parses")
    }

    /// `git count-objects -v`'s `count:` line — the loose-object total of
    /// the repo's own ODB (the number SEC-15 says must not move).
    fn loose_object_count(dir: &Path) -> u64 {
        let out = StdCommand::new("git")
            .arg("-C")
            .arg(dir)
            .args(["count-objects", "-v"])
            .output()
            .expect("git count-objects runs");
        String::from_utf8_lossy(&out.stdout)
            .lines()
            .find_map(|l| l.strip_prefix("count: "))
            .and_then(|n| n.trim().parse().ok())
            .expect("count-objects reports a count")
    }

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

    // --- parser unit tests (pure, no subprocess) --------------------------

    #[test]
    fn parse_merge_tree_reports_clean_for_a_bare_oid_and_exit_zero() {
        let stdout = b"09dbf22b812062fac5add81298938133552c48ee\n";
        let (clean, conflicts) = parse_merge_tree(0, stdout, b"").unwrap();
        assert!(clean);
        assert!(conflicts.is_empty());
    }

    #[test]
    fn parse_merge_tree_parses_a_single_conflicted_path() {
        let stdout = b"89cdede0e02a0e9c33840f149367d48d2a83bc08\na.txt\n\nAuto-merging a.txt\nCONFLICT (content): Merge conflict in a.txt\n";
        let (clean, conflicts) = parse_merge_tree(1, stdout, b"").unwrap();
        assert!(!clean);
        assert_eq!(conflicts, vec!["a.txt".to_string()]);
    }

    #[test]
    fn parse_merge_tree_parses_multiple_conflicted_paths() {
        let stdout = b"d07717d7d1f64203323c505a7d37f5c6bf87048f\na.txt\nb.txt\n\nAuto-merging a.txt\nCONFLICT (content): Merge conflict in a.txt\nAuto-merging b.txt\nCONFLICT (content): Merge conflict in b.txt\n";
        let (clean, conflicts) = parse_merge_tree(1, stdout, b"").unwrap();
        assert!(!clean);
        assert_eq!(conflicts, vec!["a.txt".to_string(), "b.txt".to_string()]);
    }

    #[test]
    fn parse_merge_tree_treats_an_unresolvable_ref_as_git_failed_not_a_conflict() {
        // Real git 2.55 shape: exit 1, EMPTY stdout, the error on stderr.
        let err = parse_merge_tree(
            1,
            b"",
            b"merge-tree: nope-does-not-exist - not something we can merge\n",
        )
        .unwrap_err();
        match err {
            HistoryError::GitFailed { status, stderr } => {
                assert_eq!(status, 1);
                assert!(stderr.contains("not something we can merge"));
            }
            other => panic!("expected GitFailed, got {other:?}"),
        }
    }

    #[test]
    fn parse_merge_tree_rejects_an_unexpected_exit_code() {
        let err = parse_merge_tree(128, b"", b"fatal: bad object\n").unwrap_err();
        assert!(matches!(err, HistoryError::GitFailed { status: 128, .. }));
    }

    // --- real subprocess (init_repo + a genuine conflicting merge) --------

    #[test]
    fn merge_check_reports_clean_for_a_fast_forwardable_pair() {
        let tmp = init_repo();
        let dir = tmp.path();
        std::fs::write(dir.join("a.txt"), "base\n").unwrap();
        git(dir, &["add", "-A"]);
        git(dir, &["commit", "-q", "-m", "base"]);
        git(dir, &["branch", "feature"]);
        git(dir, &["checkout", "-q", "feature"]);
        std::fs::write(dir.join("b.txt"), "feature\n").unwrap();
        git(dir, &["add", "-A"]);
        git(dir, &["commit", "-q", "-m", "feature change"]);

        let scratch = tempfile::tempdir().unwrap();
        let mc = merge_check(dir, scratch.path(), &rs("main"), &rs("feature")).unwrap();
        assert!(mc.clean);
        assert!(mc.conflicts.is_empty());
        assert_eq!(mc.ahead, 1);
        assert_eq!(mc.behind, 0);
        assert!(mc.resolved.merge_base.is_some());
    }

    #[test]
    fn merge_check_reports_conflicts_for_two_branches_editing_the_same_line() {
        let tmp = init_repo();
        let dir = tmp.path();
        std::fs::write(dir.join("a.txt"), "base\n").unwrap();
        git(dir, &["add", "-A"]);
        git(dir, &["commit", "-q", "-m", "base"]);
        git(dir, &["branch", "feature"]);
        git(dir, &["checkout", "-q", "feature"]);
        std::fs::write(dir.join("a.txt"), "feature change\n").unwrap();
        git(dir, &["add", "-A"]);
        git(dir, &["commit", "-q", "-m", "feature edits a.txt"]);
        git(dir, &["checkout", "-q", "main"]);
        std::fs::write(dir.join("a.txt"), "main change\n").unwrap();
        git(dir, &["add", "-A"]);
        git(dir, &["commit", "-q", "-m", "main edits a.txt"]);

        let scratch = tempfile::tempdir().unwrap();
        let objects_before = loose_object_count(dir);
        let mc = merge_check(dir, scratch.path(), &rs("main"), &rs("feature")).unwrap();
        assert!(!mc.clean);
        assert_eq!(
            mc.conflicts,
            vec![ConflictEntry {
                path: "a.txt".to_string()
            }]
        );
        assert_eq!(mc.ahead, 1);
        assert_eq!(mc.behind, 1);

        // The working tree must be untouched — this call never mutates it.
        let status = StdCommand::new("git")
            .arg("-C")
            .arg(dir)
            .args(["status", "--porcelain"])
            .output()
            .unwrap();
        assert!(
            String::from_utf8_lossy(&status.stdout).trim().is_empty(),
            "merge_check must never touch the working tree"
        );

        // SEC-15 — and it must never touch the repo's OBJECT DATABASE
        // either: `--write-tree` writes the would-be merge result, which
        // before V70-A2 landed as loose objects in the browsed repo.
        assert_eq!(
            loose_object_count(dir),
            objects_before,
            "merge_check must write no objects into the browsed repo's ODB"
        );
        // ...and the scratch dir it used is gone with the Drop guard.
        assert!(
            std::fs::read_dir(scratch.path())
                .map(|mut d| d.next().is_none())
                .unwrap_or(true),
            "the per-request scratch ODB must be removed on drop"
        );
    }

    #[test]
    fn merge_check_rejects_a_dash_prefixed_from() {
        // V70-A2: no fixture repo — the refusal moved INTO the type
        // `merge_check` takes, so a dash-prefixed revspec cannot reach the
        // fn at all. The route-visible error is the same `BadRevspec` 400.
        let err: HistoryError = Revspec::parse("--output=/tmp/x").unwrap_err().into();
        assert!(matches!(err, HistoryError::BadRevspec(_)), "got: {err:?}");
    }

    #[test]
    fn merge_check_reports_a_clean_error_for_an_unresolvable_to() {
        let tmp = init_repo();
        let dir = tmp.path();
        std::fs::write(dir.join("a.txt"), "x\n").unwrap();
        git(dir, &["add", "-A"]);
        git(dir, &["commit", "-q", "-m", "c1"]);

        let scratch = tempfile::tempdir().unwrap();
        let err = merge_check(dir, scratch.path(), &rs("main"), &rs("does-not-exist")).unwrap_err();
        assert!(
            matches!(err, HistoryError::GitFailed { .. }),
            "got: {err:?}"
        );
    }
}
