//! Time-first-class routes (Phase C-server — "The Operable Reader"):
//! `GET /api/commit` (the commit page hub), `GET /api/compare` (repo-level
//! compare), `GET /api/branches` (ahead/behind + session attribution), and
//! `GET /api/file-history` (per-file history). Each submodule owns one
//! endpoint's git-shelling business logic; `routes.rs` stays a thin HTTP
//! wrapper (params + `ApiError` mapping), exactly like `diff.rs`/`blame`/
//! `checkout` before it.
//!
//! This is this crate's SIXTH git-subprocess wrapper — see `diff.rs`'s
//! module doc ("Why `DiffError` isn't shared with its siblings") for the
//! five that came before. [`HistoryError`] is its own enum, deliberately
//! not unified with those — same rationale. It IS, however, shared ACROSS
//! this module's four submodules (`commit`/`compare`/`branches`/
//! `file_history`): they're one conceptual wrapper (the "time" reading
//! surface), just split into files for size, not five independent
//! subsystems that each reason about a genuinely different failure
//! surface.
//!
//! The actual numstat/name-status TOKEN parsing (shared with
//! `sessiondiff::git_diff`'s existing, narrower need) lives in
//! `crate::numstat` — a separate, error-enum-free PURE parsing module (see
//! its own doc for why it carries no I/O/error type). [`diff_files`] below
//! is the glue: it spawns the two subprocesses (this module's own
//! `HistoryError`) and hands their stdout to `crate::numstat::merge`.
//!
//! Every subprocess call here is genuinely BLOCKING (`std::process::
//! Command`, synchronous stdout read) — route handlers in `routes.rs` MUST
//! run each one inside `spawn_blocking`, same discipline as `diff::
//! diff_file`/`blame::blame_file`/`checkout::switch_repo`.

pub mod branches;
pub mod commit;
pub mod compare;
/// V75-M3 (D15) — `branch-facts/1`: the ONE `for-each-ref` pass, the
/// classed base ladder, and the view/stale/merged/agent rules.
pub mod facts;
pub mod file_history;
pub mod merge_check;
/// V75-M3 (D15) — the conflict radar: `merge_check`'s mechanism fanned out
/// over N candidate branches, with a hard pair cap and a hunk-probe budget.
pub mod radar;
pub mod range_diff;
/// V70-A2 (SEC-15) — the per-request scratch object directory `merge-tree
/// --write-tree` writes into, plus its boot-time orphan sweep.
pub mod scratch;
/// V76-R3d — `scrub/1`: file-scoped time stops and nearest-prior `?at=`.
pub mod scrub;
pub mod stacks;

use crate::numstat::FileChange;
use std::path::Path;
use std::process::Command;

#[derive(Debug, thiserror::Error)]
pub enum HistoryError {
    #[error("failed to spawn git: {0}")]
    Spawn(std::io::Error),
    #[error("git failed (exit {status}): {stderr}")]
    GitFailed { status: i32, stderr: String },
    /// A caller-supplied revspec (`compare`'s `from`/`to`) reached here
    /// starting with `-` — the SAME argument-injection class
    /// `diff::DiffError::BadRevspec`/`checkout::CheckoutError::BadTarget`
    /// each guard against (a leading dash makes git's own option parser
    /// treat an argv entry as a flag rather than a revspec, even though
    /// this spawn never goes through a shell) — separate enum, identical
    /// guard.
    #[error("revspec must not start with '-': {0:?}")]
    BadRevspec(String),
    /// `commit`'s `sha` passed shape validation (`join::ladder::
    /// is_plausible_sha`) but didn't resolve to any commit in this repo —
    /// a distinct variant from `GitFailed` so `routes.rs` can map this to
    /// 404 (an honest "no such commit") rather than `GitFailed`'s 400
    /// ("malformed request").
    #[error("commit not found: {0:?}")]
    NotFound(String),
    /// V75-M3 — the per-request scratch object directory (SEC-15,
    /// [`scratch::ScratchOdb`]) could not be created. This is the
    /// READ-ONLY-MOUNT refusal, and it is deliberately its own variant:
    /// the browsed repo needs no write access (that is exactly what the
    /// scratch redirection bought), so the only thing a `merge-tree` lane
    /// can fail on for want of a writable directory is the DAEMON'S OWN
    /// state dir — and telling a caller "git failed" for that would send
    /// them looking in the wrong place. `routes.rs` maps it to a typed
    /// `503` naming the directory (never the io error's raw text).
    ///
    /// The io error's text rides `reason`, NOT a field named `source`:
    /// `thiserror` treats `source` as the `std::error::Error` source and
    /// requires it to BE an error, which a captured `String` is not.
    #[error("scratch object directory is not writable: {path} ({reason})")]
    ScratchUnwritable { path: String, reason: String },
}

pub type Result<T> = std::result::Result<T, HistoryError>;

/// The argument-injection gate for an INTERNALLY-derived revspec — a
/// branch name this daemon read out of `git for-each-ref`, not a caller's
/// query param.
///
/// V70-A2 (SEC-17) narrowed this fn's job. Every CALLER-supplied revspec
/// now arrives as a [`crate::git::Revspec`] or [`crate::git::RefRange`],
/// whose only constructor is the full validator — so `compare`,
/// `merge_check`, `range_diff`, `ahead_behind` and `layer_diff` no longer
/// call this at all. What is left is the case a type cannot cover: git
/// PERMITS a ref whose name starts with `-`, so a name enumerated from the
/// repo itself (`stacks::is_stale`/`detect_base`) is still a
/// repo-controlled string reaching argv, and still gets this check.
pub(crate) fn reject_dash_prefixed(revspec: &str) -> Result<()> {
    if revspec.starts_with('-') {
        return Err(HistoryError::BadRevspec(revspec.to_string()));
    }
    Ok(())
}

/// V70-A2 (SEC-17) — a `RevspecError` from a route's own `Revspec::parse`
/// folds into this module's error type, so the wire shape of a rejected
/// `?from=`/`?old=` is byte-identical to the `BadRevspec` 400 the
/// per-module dash guards used to produce.
impl From<crate::git::RevspecError> for HistoryError {
    fn from(e: crate::git::RevspecError) -> Self {
        HistoryError::BadRevspec(e.0)
    }
}

/// Run `git -C repo_root <args>`, returning raw stdout bytes on a zero
/// exit or [`HistoryError::GitFailed`] otherwise. Shared by every
/// submodule below.
pub(crate) fn run_git_raw(repo_root: &Path, args: &[&str]) -> Result<Vec<u8>> {
    let output = Command::new("git")
        .arg("-C")
        .arg(repo_root)
        .args(args)
        .output()
        .map_err(HistoryError::Spawn)?;
    if !output.status.success() {
        return Err(HistoryError::GitFailed {
            status: output.status.code().unwrap_or(-1),
            stderr: String::from_utf8_lossy(&output.stderr).trim().to_string(),
        });
    }
    Ok(output.stdout)
}

/// The shared numstat+name-status merge (see the module doc): runs
/// `git -C repo_root <subcmd> --numstat -z <args>` and
/// `git -C repo_root <subcmd> --name-status -z <args>` (the SAME `args`
/// both times — same repo, same revspec, same `-M` — so the two runs
/// agree entry-for-entry, which `crate::numstat::parse_numstat_z` depends
/// on), then merges them via `crate::numstat::merge`. Used by
/// [`commit::commit_files`] (`subcmd = "diff-tree"`) and
/// [`compare::compare`] (`subcmd = "diff"`).
pub(crate) fn diff_files(repo_root: &Path, subcmd: &str, args: &[&str]) -> Result<Vec<FileChange>> {
    let mut numstat_args = vec![subcmd, "--numstat", "-z"];
    numstat_args.extend_from_slice(args);
    let mut name_status_args = vec![subcmd, "--name-status", "-z"];
    name_status_args.extend_from_slice(args);

    let numstat_out = run_git_raw(repo_root, &numstat_args)?;
    let name_status_out = run_git_raw(repo_root, &name_status_args)?;

    let name_status = crate::numstat::parse_name_status_z(&name_status_out);
    let rename_flags: Vec<bool> = name_status
        .iter()
        .map(|(status, _, _)| status.starts_with('R') || status.starts_with('C'))
        .collect();
    let numstat = crate::numstat::parse_numstat_z(&numstat_out, &rename_flags);
    Ok(crate::numstat::merge(name_status, numstat))
}

// --- Shared wire types ---------------------------------------------------

/// `{name, email, time}` — one identity + timestamp, shared by `commit`'s
/// `author`/`committer` fields. The one place this crate needs the FULL
/// breakdown (elsewhere, e.g. `CommitSummary` below or `kb_core::vcs::
/// ResolvedCommit`/`sessiondiff`'s own `CommitEntryOut`, a plain
/// `"Name <email>"` display string is enough).
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct Person {
    pub name: String,
    pub email: String,
    /// Unix seconds.
    pub time: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct Trailer {
    pub key: String,
    pub value: String,
}

/// One commit summary row — `compare`'s `commits[]` and `file_history`'s
/// `entries[]` share this EXACT shape (both are "which commits, in what
/// order" lists, never the full metadata `commit`'s own page needs).
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct CommitSummary {
    pub sha: String,
    pub subject: String,
    /// `"Name <email>"` — a plain display string (unlike `commit`'s own
    /// `author`/`committer`, a summary row has no separate use for the
    /// name/email/time split; `author_time` below already carries the
    /// timestamp).
    pub author: String,
    pub author_time: i64,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, serde::Serialize)]
pub struct FileTotals {
    pub files: usize,
    pub insertions: u32,
    pub deletions: u32,
}

pub(crate) fn totals_for(files: &[FileChange]) -> FileTotals {
    FileTotals {
        files: files.len(),
        insertions: files.iter().map(|f| f.insertions).sum(),
        deletions: files.iter().map(|f| f.deletions).sum(),
    }
}

/// `git log --format=<LOG_SUMMARY_FMT>` — one line per commit, `%x1f`
/// (unit separator) between fields. `%s` never contains an embedded
/// newline (git's own guarantee for a subject line), so splitting the
/// whole subprocess stdout on `\n` and then each line on `\x1f` is
/// unambiguous — same technique `kb_core::vcs::resolve_commit` already
/// uses for its own `%x1f`-joined format.
pub(crate) const LOG_SUMMARY_FMT: &str = "%H%x1f%s%x1f%an <%ae>%x1f%at";

/// Parse one [`LOG_SUMMARY_FMT`] line into a [`CommitSummary`] — shared by
/// `compare::commit_list` and `file_history::file_history`.
pub(crate) fn parse_log_summary_line(line: &str) -> Option<CommitSummary> {
    let mut f = line.splitn(4, '\u{1f}');
    let sha = f.next()?.to_string();
    let subject = f.next()?.to_string();
    let author = f.next()?.to_string();
    let author_time: i64 = f.next()?.parse().ok()?;
    Some(CommitSummary {
        sha,
        subject,
        author,
        author_time,
    })
}

/// "Which shas did each side resolve to, and do they have a common
/// ancestor" — shared by [`compare::compare`] (Phase C2) and
/// [`merge_check::merge_check`] (Phase G): both need to tell a caller
/// exactly what `from`/`to` resolved to before doing anything else with
/// them. Originally `compare`'s own private type; hoisted here once
/// `merge_check` needed the identical shape, rather than a second copy.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct Resolved {
    pub from_sha: String,
    pub to_sha: String,
    /// `None` when `from`/`to` share no common ancestor (disjoint
    /// histories) — degraded, never an error (see `compare`'s module doc).
    pub merge_base: Option<String>,
}

/// `git rev-parse --verify <spec>^{commit}` — the full, disambiguated sha
/// one side of a compare/merge-check resolves to. Shared by `compare` and
/// `merge_check`.
pub(crate) fn resolve_sha(repo_root: &Path, spec: &str) -> Result<String> {
    let out = run_git_raw(
        repo_root,
        &["rev-parse", "--verify", &format!("{spec}^{{commit}}")],
    )?;
    Ok(String::from_utf8_lossy(&out).trim().to_string())
}

/// V75-M3 — `git rev-parse --verify <spec>^{commit}` as an OPTION rather
/// than a `Result`. Same call as [`resolve_sha`]; the difference is the
/// contract at the call site: `branch-facts/1` asks "does this ref name a
/// commit right now?" about refs it enumerated itself, where "no" is an
/// ordinary answer (an unborn HEAD, a symref with no target) and not a
/// request error to propagate. `pub` because `crate::branches` is that
/// caller.
pub fn resolve_ref_commit(repo_root: &Path, spec: &str) -> Option<String> {
    let spec = crate::git::Revspec::parse(spec).ok()?;
    let out = resolve_sha(repo_root, spec.as_str()).ok()?;
    (!out.is_empty()).then_some(out)
}

/// V75-M3 — `merge_base` for a caller-supplied REF rather than two
/// resolved shas. `pub` for `crate::branches`, and it validates the ref
/// through `Revspec` first: `merge_base` below takes shas this daemon
/// resolved itself, so its own argv carries no validator.
pub fn merge_base_of(repo_root: &Path, spec: &str, to_sha: &str) -> Option<String> {
    let spec = crate::git::Revspec::parse(spec).ok()?;
    merge_base(repo_root, spec.as_str(), to_sha)
}

/// `git merge-base <from> <to>` — `None` (not an error) on a non-zero exit
/// (no common ancestor). Shared by `compare` and `merge_check`.
pub(crate) fn merge_base(repo_root: &Path, from_sha: &str, to_sha: &str) -> Option<String> {
    let output = Command::new("git")
        .arg("-C")
        .arg(repo_root)
        .args(["merge-base", from_sha, to_sha])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let s = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if s.is_empty() {
        None
    } else {
        Some(s)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reject_dash_prefixed_rejects_only_a_leading_dash() {
        assert!(reject_dash_prefixed("main").is_ok());
        assert!(reject_dash_prefixed("HEAD~3").is_ok());
        assert!(reject_dash_prefixed("--output=/tmp/x").is_err());
        assert!(reject_dash_prefixed("-U9999").is_err());
    }

    #[test]
    fn parse_log_summary_line_splits_four_fields() {
        let line = "abc123\u{1f}the subject\u{1f}Test <test@example.com>\u{1f}1700000000";
        let got = parse_log_summary_line(line).unwrap();
        assert_eq!(got.sha, "abc123");
        assert_eq!(got.subject, "the subject");
        assert_eq!(got.author, "Test <test@example.com>");
        assert_eq!(got.author_time, 1_700_000_000);
    }

    #[test]
    fn parse_log_summary_line_rejects_a_malformed_line() {
        assert!(parse_log_summary_line("too\u{1f}few").is_none());
    }

    #[test]
    fn totals_for_sums_insertions_and_deletions() {
        let files = vec![
            FileChange {
                path: "a.txt".to_string(),
                old_path: None,
                insertions: 3,
                deletions: 1,
                binary: false,
                status: "M".to_string(),
            },
            FileChange {
                path: "b.txt".to_string(),
                old_path: None,
                insertions: 0,
                deletions: 0,
                binary: true,
                status: "A".to_string(),
            },
        ];
        let totals = totals_for(&files);
        assert_eq!(totals.files, 2);
        assert_eq!(totals.insertions, 3);
        assert_eq!(totals.deletions, 1);
    }
}
