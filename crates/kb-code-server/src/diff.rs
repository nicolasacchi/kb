//! `GET /api/diff` (W4.2 — the reader's diff view): a single file's unified
//! diff between two refs, or one ref and the CURRENT WORKING TREE. Shells
//! out to a real `git diff` subprocess (ADR-4 — the same precedent
//! `sessiondiff::git_diff`, `blame::incremental`, and
//! `mirror::reconcile::run_git_diff` already established: shell out for
//! diff-shaped work rather than reimplementing git's own diff algorithm).
//!
//! Unlike `sessiondiff::git_diff::commit_numstat` (per-file insertion/
//! deletion COUNTS for a whole commit), this module returns the file's full
//! unified-diff TEXT (`diff --git` header + `@@` hunk headers + context/
//! added/removed lines) for exactly one path — the SPA parses that text
//! client-side into hunks for rendering (`web-code/src/lib/diff.ts`,
//! golden-tested there) rather than the server pre-structuring it into a
//! hunk array. That keeps this module a thin, directly-testable wrapper
//! (assert on substrings of the raw text, same style as
//! `sessiondiff::git_diff`'s own tests) and avoids inventing a second
//! diff-hunk JSON shape server-side.
//!
//! BLOCKING — a real subprocess spawn + synchronous stdout read. Callers
//! MUST run [`diff_file`] inside `spawn_blocking`, exactly like
//! `blame::incremental::run_streaming` and `sessiondiff::git_diff`'s own
//! docs require.
//!
//! # Why `DiffError` isn't shared with its siblings
//!
//! This crate has SIX git-subprocess wrappers, each with its own error
//! enum: this module's [`DiffError`], `checkout::CheckoutError`,
//! `blame::BlameError` (+ `blame::incremental::IncrementalError`),
//! `sessiondiff::git_diff::DiffError`, and `history::HistoryError`
//! (`mirror::reconcile`'s own subprocess call is the one exception — it
//! has no error enum at all, see that module's doc). That's a DELIBERATE
//! per-module duplication, not an oversight: each wrapper's variant set
//! tracks a genuinely different failure surface it alone has to reason
//! about — THIS one's `BadRevspec` exists because `GET /api/diff` forwards
//! caller-supplied `from`/`to` revspecs to `git diff`'s own argv (an
//! argument-injection gate no other wrapper needs in the same shape:
//! `checkout::CheckoutError::BadTarget` is the analogous guard for
//! `switch`/`checkout`'s `target`, `history::HistoryError::BadRevspec` the
//! analogous guard for `GET /api/compare`'s `from`/`to`, and
//! `history::HistoryError::NotFound` exists purely for `GET /api/commit`'s
//! "sha well-formed but unresolvable" → 404 case no sibling wrapper needs;
//! `sessiondiff::git_diff::DiffError` has no such variant at all because its
//! caller only ever feeds it shas ALREADY resolved by `join::local`, never
//! raw user input). A single shared `GitSubprocessError` would either grow
//! a union of every wrapper's variants (most callers matching on variants
//! that can never occur for them) or erase the per-route HTTP-mapping
//! precision `routes.rs`'s `From` impls rely on (e.g. this module's
//! `GitFailed` maps to 400, `checkout::CheckoutError::Dirty` maps to a
//! structured 409) — six small, self-contained enums cost far less than
//! that coupling. (The actual numstat/name-status TOKEN parsing IS shared
//! across wrapper boundaries, though — see `crate::numstat`'s module doc —
//! because that logic carries no error type of its own for this doc's
//! concern to apply to.)

use crate::git::Revspec;
use std::path::Path;
use std::process::Command;

#[derive(Debug, thiserror::Error)]
pub enum DiffError {
    #[error("failed to spawn git: {0}")]
    Spawn(std::io::Error),
    #[error("git diff failed (exit {status}): {stderr}")]
    GitFailed { status: i32, stderr: String },
    /// `from`/`to` reached here starting with `-` — passed as a raw argv
    /// entry to `git diff`, where a leading dash makes git option-parse it
    /// (e.g. `--output=<path>` writes the diff to an arbitrary file,
    /// `-U9999` is merely odd but the shape is the same) EVEN THOUGH this
    /// spawn never goes through a shell: `Command::args` hands git argv
    /// entries directly, and git's own arg parser doesn't distinguish "a
    /// caller-supplied revspec" from "an option" by position alone.
    /// `GET /api/diff` is reachable over the ordinary `auth_bearer` gate
    /// (not loopback-only), so this is rejected before the subprocess ever
    /// spawns — see `diff_file`'s doc.
    #[error("revspec must not start with '-': {0:?}")]
    BadRevspec(String),
}

pub type Result<T> = std::result::Result<T, DiffError>;

/// `git -C <repo_root> diff --no-color -U3 <from> [<to>] -- <path>` —
/// unified diff text for one file. `to` given: `from`..`to` (both resolved
/// as git revspecs — commits, branches, tags). `to` omitted: `from` vs. the
/// CURRENT WORKING TREE (git's own one-arg-`diff` convention — mirrors
/// `GET /api/file`'s "no `ref` = working tree" default, invariant of
/// least-surprise across the reader's ref-browsing surface). Empty string
/// output means "no textual difference" between the two sides for this
/// path (identical content, or the path is absent on both) — this fn does
/// not distinguish that from "path never existed at either side"; a caller
/// that needs the distinction can check `GitRepo::blob_oid` separately. A
/// binary file's diff is git's own `Binary files a/... and b/... differ`
/// one-liner — passed through verbatim, not specially parsed.
///
/// `from`/`to` are [`Revspec`]s — the type whose only constructor is the
/// injection gate ([`crate::git::revspec`]), so a malicious revspec is
/// rejected at the ROUTE, before this fn is even reachable.
pub fn diff_file(
    repo_root: &Path,
    from: &Revspec,
    to: Option<&Revspec>,
    path: &str,
) -> Result<String> {
    // V70-A2 (SEC-17) — `from`/`to` arrive already validated: `Revspec`'s
    // only constructor IS the injection gate, so there is no guard to
    // forget here and no way to call this fn with a raw `String`.
    let mut args: Vec<&str> = vec!["diff", "--no-color", "-U3", from.as_str()];
    if let Some(to) = to {
        args.push(to.as_str());
    }
    // `--` before the pathspec: a path can never be re-read as a revspec.
    args.push("--");
    args.push(path);

    let output = Command::new("git")
        .arg("-C")
        .arg(repo_root)
        .args(&args)
        .output()
        .map_err(DiffError::Spawn)?;
    // `git diff` exits 0 (no difference) or 1 (differences found) on
    // success; anything else (128, a malformed revspec, etc.) is a real
    // failure. Mirror `sessiondiff::git_diff::run_git`'s stricter
    // `status.success()` check would reject the (extremely common) exit-1
    // "there is a diff" case, so this checks the code directly instead.
    match output.status.code() {
        Some(0) | Some(1) => Ok(String::from_utf8_lossy(&output.stdout).into_owned()),
        other => Err(DiffError::GitFailed {
            status: other.unwrap_or(-1),
            stderr: String::from_utf8_lossy(&output.stderr).trim().to_string(),
        }),
    }
}

/// V70-A2 (SEC-17) — a `RevspecError` from a route's own
/// `Revspec::parse` folds into this module's error type, so the wire shape
/// of a rejected `?from=`/`?to=` is byte-identical to the pre-V70-A2
/// `BadRevspec` 400 the local `reject_dash_prefixed` produced. That
/// function is GONE: its predicate now lives in exactly one place
/// (`git::revspec::Revspec::parse`), which is the whole point of the
/// newtype.
impl From<crate::git::RevspecError> for DiffError {
    fn from(e: crate::git::RevspecError) -> Self {
        DiffError::BadRevspec(e.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::Command as StdCommand;

    /// Test-local shorthand: every fixture revspec here is a sha or a
    /// branch name this test just minted, so `parse` cannot fail.
    fn rs(s: &str) -> Revspec {
        Revspec::parse(s).expect("fixture revspec parses")
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
    fn diff_between_two_refs_reports_the_hunk() {
        let tmp = init_repo();
        let dir = tmp.path();
        std::fs::write(dir.join("a.txt"), "line1\nline2\nline3\n").unwrap();
        git(dir, &["add", "a.txt"]);
        git(dir, &["commit", "-q", "-m", "c1"]);
        let sha1 = git_out(dir, &["rev-parse", "HEAD"]);

        std::fs::write(dir.join("a.txt"), "line1\nCHANGED\nline3\n").unwrap();
        git(dir, &["add", "a.txt"]);
        git(dir, &["commit", "-q", "-m", "c2"]);
        let sha2 = git_out(dir, &["rev-parse", "HEAD"]);

        let diff = diff_file(dir, &rs(&sha1), Some(&rs(&sha2)), "a.txt").unwrap();
        assert!(diff.contains("@@"), "expected a hunk header, got: {diff}");
        assert!(diff.contains("-line2"), "got: {diff}");
        assert!(diff.contains("+CHANGED"), "got: {diff}");
    }

    #[test]
    fn diff_with_no_to_compares_against_the_working_tree() {
        let tmp = init_repo();
        let dir = tmp.path();
        std::fs::write(dir.join("a.txt"), "line1\n").unwrap();
        git(dir, &["add", "a.txt"]);
        git(dir, &["commit", "-q", "-m", "c1"]);
        let sha1 = git_out(dir, &["rev-parse", "HEAD"]);

        // Uncommitted working-tree edit, never staged/committed.
        std::fs::write(dir.join("a.txt"), "line1\nuncommitted\n").unwrap();

        let diff = diff_file(dir, &rs(&sha1), None, "a.txt").unwrap();
        assert!(diff.contains("+uncommitted"), "got: {diff}");
    }

    #[test]
    fn diff_with_no_difference_is_empty() {
        let tmp = init_repo();
        let dir = tmp.path();
        std::fs::write(dir.join("a.txt"), "line1\n").unwrap();
        git(dir, &["add", "a.txt"]);
        git(dir, &["commit", "-q", "-m", "c1"]);
        let sha1 = git_out(dir, &["rev-parse", "HEAD"]);

        let diff = diff_file(dir, &rs(&sha1), None, "a.txt").unwrap();
        assert_eq!(diff, "");
    }

    #[test]
    fn diff_on_an_unresolvable_ref_errors_cleanly() {
        let tmp = init_repo();
        std::fs::write(tmp.path().join("a.txt"), "x\n").unwrap();
        git(tmp.path(), &["add", "a.txt"]);
        git(tmp.path(), &["commit", "-q", "-m", "c1"]);

        let err = diff_file(tmp.path(), &rs("deadbeefdeadbeefdead"), None, "a.txt").unwrap_err();
        assert!(matches!(err, DiffError::GitFailed { .. }));
    }

    #[test]
    fn diff_rejects_a_dash_prefixed_from_before_ever_spawning_git() {
        let tmp = init_repo();
        let dir = tmp.path();
        std::fs::write(dir.join("a.txt"), "line1\n").unwrap();
        git(dir, &["add", "a.txt"]);
        git(dir, &["commit", "-q", "-m", "c1"]);

        // A file OUTSIDE the repo — if git ever ran, `--output=<path>`
        // would be parsed as an option and this file would exist.
        let outside = tempfile::tempdir().unwrap();
        let target = outside.path().join("pwned.txt");
        let malicious_from = format!("--output={}", target.display());

        // V70-A2: the refusal now happens in `Revspec::parse` — the type
        // that `diff_file` takes — so git is unreachable BY CONSTRUCTION
        // rather than by a guard the fn remembers to call. The error the
        // route surfaces is the same `BadRevspec` 400 as before.
        let err: DiffError = Revspec::parse(&malicious_from).unwrap_err().into();
        assert!(matches!(err, DiffError::BadRevspec(_)), "got: {err:?}");
        assert!(
            !target.exists(),
            "git must never have spawned: {target:?} was created"
        );
    }

    #[test]
    fn diff_rejects_a_dash_prefixed_to() {
        // V70-A2: no fixture repo — `diff_file` takes `Revspec`s, so a
        // dash-prefixed `to` is refused by the CONSTRUCTOR and this test
        // has no subprocess left to guard against. The error the route
        // surfaces is the same `BadRevspec` 400 as before.
        let err: DiffError = Revspec::parse("-U9999").unwrap_err().into();
        assert!(matches!(err, DiffError::BadRevspec(_)), "got: {err:?}");
    }

    #[test]
    fn diff_reports_a_binary_file_change_without_line_content() {
        let tmp = init_repo();
        let dir = tmp.path();
        std::fs::write(dir.join("bin.dat"), [0u8, 1, 2, 0, 255]).unwrap();
        git(dir, &["add", "bin.dat"]);
        git(dir, &["commit", "-q", "-m", "c1"]);
        let sha1 = git_out(dir, &["rev-parse", "HEAD"]);

        std::fs::write(dir.join("bin.dat"), [0u8, 1, 2, 0, 254, 253]).unwrap();
        git(dir, &["add", "bin.dat"]);
        git(dir, &["commit", "-q", "-m", "c2"]);
        let sha2 = git_out(dir, &["rev-parse", "HEAD"]);

        let diff = diff_file(dir, &rs(&sha1), Some(&rs(&sha2)), "bin.dat").unwrap();
        assert!(diff.contains("Binary files"), "got: {diff}");
    }
}
