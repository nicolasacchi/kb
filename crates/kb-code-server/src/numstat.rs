//! Shared, PURE (no I/O, no error type of its own) git numstat/name-status
//! TOKEN parsing — the ONE merge helper for "per-file insertion/deletion
//! counts + status" this crate needs, reused by:
//!
//! - `sessiondiff::git_diff::commit_numstat`'s existing, narrower need
//!   (insertions/deletions/binary only, no status/rename — the session-diff
//!   feature's own `FileStat` never surfaced a status column) — its private
//!   line parser now funnels its counts-parsing step through
//!   [`parse_counts`] rather than re-deriving the `"-"`-means-binary rule a
//!   second time.
//! - `history::{commit,compare}` (this crate's Phase-C "time" routes),
//!   which additionally need A/M/D/R/C status + the old path for a rename —
//!   via [`parse_name_status_z`] + [`parse_numstat_z`] + [`merge`].
//!
//! This module deliberately owns NO subprocess call and NO error enum: each
//! caller spawns `git` itself with ITS OWN error type (matching this
//! crate's "five/six separate git-subprocess wrappers, each with its own
//! error enum" convention — see `diff.rs`'s module doc), then hands the
//! already-collected stdout bytes here for parsing. That keeps the ACTUAL
//! parsing logic — the part worth not duplicating a third time — in one
//! place, without forcing every caller's error handling through a shared
//! type it doesn't otherwise need.
//!
//! # Why `-z` for the structured (`history`) side
//!
//! Git's default (newline-terminated) `--numstat` abbreviates a rename as
//! `<ins>\t<del>\told/{a => b}/path` (a single, awkward, hard-to-split-back-
//! apart string) and `--name-status` as `R100\told\tnew` — different
//! shapes, and numstat's own can't be un-abbreviated without `-z`.
//!
//! With `-z`, `--name-status` NUL-terminates EVERY field uniformly: each
//! entry is `status\0path\0` (plain) or `status\0old\0new\0` (rename/
//! copy) — always 1 or 2 NUL-separated path tokens after the status.
//! `--numstat -z`, however, is NOT symmetric with that: a PLAIN entry is
//! ONE NUL-terminated record with the path still TAB-joined onto the counts
//! (`ins\tdel\tpath\0` — three tab/NUL-shaped fields, ONE token by NUL
//! splitting), and ONLY a rename/copy switches to the NUL-separated
//! two-path shape (`ins\tdel\0old\0new\0` — three separate tokens). Verified
//! empirically against a real `git` (2.55) — this asymmetry is not
//! documented as prominently as the shapes themselves. So [`parse_numstat_z`]
//! branches per entry on `rename_flags` (one bool per entry, read off a
//! PARALLEL `--name-status -z` run over the exact same repo/revspec/`-M`):
//! a plain entry's counts+path arrive in ONE token (split further on `\t`,
//! keeping the path embedded); a rename/copy's counts arrive in their own
//! clean token, followed by two separate path tokens to skip. This only
//! works because both runs see the same diff (same repository state, same
//! revspec, same rename-detection flag), so they agree entry-for-entry, in
//! order.

/// One file's merged numstat + name-status row. `Serialize`d directly into
/// `commit/1`'s and `compare/1`'s `files[]` (`crate::history::commit`/
/// `crate::history::compare`) — unlike `sessiondiff::git_diff::FileStat`
/// (which has its own distinct wire type, `sessiondiff::CommitFileOut`, "so
/// `git_diff` itself carries no `serde` dependency of its own" per that
/// module's doc), `crate::history`'s routes serialize this type as-is: it
/// was designed as this crate's wire shape for a file change from the
/// start (unlike `FileStat`, which predates any of `history`'s routes).
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct FileChange {
    /// The current (post-change) path — repo-relative, forward-slash.
    pub path: String,
    /// `Some(old path)` for a rename/copy (git status `R`/`C`); `None`
    /// otherwise. An ADDITIVE field beyond the endpoints' literal
    /// `{path, insertions, deletions, binary, status}` contract (a
    /// rename/copy's origin is otherwise unrecoverable from the response)
    /// — omitted from the wire entirely when absent.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub old_path: Option<String>,
    pub insertions: u32,
    pub deletions: u32,
    /// `true` when git reported `-\t-` for this file's counts (a binary
    /// diff — git never counts line deltas for one).
    pub binary: bool,
    /// The FIRST character of git's name-status code (`A`, `M`, `D`, `R`,
    /// `C`, `T`, `U`, `X`, `B`, ...) — a rename/copy's similarity-score
    /// suffix (e.g. `R100`'s `100`) is dropped; no caller of this module
    /// needs it.
    pub status: String,
}

/// `<ins>\t<del>` (or `-\t-` for a binary file) → `(insertions, deletions,
/// binary)`. The ONE counts-parsing routine every numstat-shaped output —
/// whether newline/tab-framed (`sessiondiff::git_diff`'s existing form) or
/// NUL/`-z`-framed (this module's own) — funnels through, so "a binary
/// file's counts are reported as a literal `-`" is defined in exactly one
/// place.
pub fn parse_counts(ins: &str, del: &str) -> Option<(u32, u32, bool)> {
    if ins == "-" && del == "-" {
        return Some((0, 0, true));
    }
    Some((ins.parse().ok()?, del.parse().ok()?, false))
}

/// Split a `-z`-terminated subprocess stdout into its NUL-separated
/// fields, dropping the trailing empty field the final terminator leaves
/// behind. Lossy UTF-8 (matches this crate's other subprocess wrappers,
/// e.g. `diff::diff_file`) — a path that isn't valid UTF-8 is vanishingly
/// rare in practice and not worth threading `OsString` through this whole
/// module for.
fn split_nul(stdout: &[u8]) -> Vec<String> {
    String::from_utf8_lossy(stdout)
        .split('\0')
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .collect()
}

/// Parse `git ... --name-status -z` output into `(status, old_path, path)`
/// triples, in order — `old_path` is `Some` exactly when `status` starts
/// with `R` or `C` (git's own two-path shape for a rename/copy).
pub fn parse_name_status_z(stdout: &[u8]) -> Vec<(String, Option<String>, String)> {
    let tokens = split_nul(stdout);
    let mut out = Vec::new();
    let mut i = 0;
    while i < tokens.len() {
        let status = tokens[i].clone();
        i += 1;
        let is_rename_or_copy = status.starts_with('R') || status.starts_with('C');
        if is_rename_or_copy {
            let old = tokens.get(i).cloned().unwrap_or_default();
            i += 1;
            let new = tokens.get(i).cloned().unwrap_or_default();
            i += 1;
            out.push((status, Some(old), new));
        } else {
            let path = tokens.get(i).cloned().unwrap_or_default();
            i += 1;
            out.push((status, None, path));
        }
    }
    out
}

/// Parse `git ... --numstat -z` output into `(insertions, deletions,
/// binary)` triples, in order. `rename_flags[i]` (from the PARALLEL
/// `--name-status -z` run, see the module doc) tells this whether entry
/// `i` is a rename/copy — which changes how many tokens it occupies AND
/// how the counts are framed within them: a PLAIN entry's counts and path
/// share ONE NUL-terminated token (`ins\tdel\tpath`, split further on
/// `\t`); a rename/copy's counts arrive in their OWN clean token
/// (`ins\tdel`), followed by two separate path tokens this fn skips (their
/// content is already known from the parallel name-status parse).
pub fn parse_numstat_z(stdout: &[u8], rename_flags: &[bool]) -> Vec<(u32, u32, bool)> {
    let tokens = split_nul(stdout);
    let mut out = Vec::new();
    let mut i = 0;
    let mut idx = 0;
    while i < tokens.len() {
        let is_rename = rename_flags.get(idx).copied().unwrap_or(false);
        let counts_token = &tokens[i];
        i += 1;
        let (ins, del) = if is_rename {
            // Clean "ins\tdel" token; two more path tokens follow (skip).
            let mut parts = counts_token.splitn(2, '\t');
            let ins = parts.next().unwrap_or("").to_string();
            let del = parts.next().unwrap_or("").to_string();
            i += 2;
            (ins, del)
        } else {
            // "ins\tdel\tpath" all in this ONE token — the path stays
            // embedded (this fn only reports counts; the path already
            // came from the parallel name-status parse).
            let mut parts = counts_token.splitn(3, '\t');
            let ins = parts.next().unwrap_or("").to_string();
            let del = parts.next().unwrap_or("").to_string();
            (ins, del)
        };
        let (insertions, deletions, binary) = parse_counts(&ins, &del).unwrap_or((0, 0, false));
        out.push((insertions, deletions, binary));
        idx += 1;
    }
    out
}

/// Zip a name-status parse with its PARALLEL numstat parse (same repo,
/// same revspec, same `-M` — see the module doc) into the merged per-file
/// view. Entries pair up positionally; a length mismatch (which should
/// never happen given matching invocations) silently truncates to the
/// shorter of the two rather than panicking.
pub fn merge(
    name_status: Vec<(String, Option<String>, String)>,
    numstat: Vec<(u32, u32, bool)>,
) -> Vec<FileChange> {
    name_status
        .into_iter()
        .zip(numstat)
        .map(
            |((status, old_path, path), (insertions, deletions, binary))| FileChange {
                path,
                old_path,
                insertions,
                deletions,
                binary,
                status: status.chars().next().map(String::from).unwrap_or_default(),
            },
        )
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_counts_reads_normal_and_binary_markers() {
        assert_eq!(parse_counts("3", "1"), Some((3, 1, false)));
        assert_eq!(parse_counts("-", "-"), Some((0, 0, true)));
        assert_eq!(parse_counts("nope", "1"), None);
    }

    fn nul_join(fields: &[&str]) -> Vec<u8> {
        let mut out = Vec::new();
        for f in fields {
            out.extend_from_slice(f.as_bytes());
            out.push(0);
        }
        out
    }

    #[test]
    fn parse_name_status_z_splits_plain_and_rename_entries() {
        let stdout = nul_join(&["M", "a.txt", "R100", "old.txt", "new.txt", "A", "b.txt"]);
        let entries = parse_name_status_z(&stdout);
        assert_eq!(
            entries,
            vec![
                ("M".to_string(), None, "a.txt".to_string()),
                (
                    "R100".to_string(),
                    Some("old.txt".to_string()),
                    "new.txt".to_string()
                ),
                ("A".to_string(), None, "b.txt".to_string()),
            ]
        );
    }

    #[test]
    fn parse_numstat_z_uses_rename_flags_to_consume_the_right_path_count() {
        // Same logical entries as the name-status test above: plain,
        // rename (2 paths), plain. A PLAIN entry's counts+path are ONE
        // NUL-terminated, tab-joined token (`"1\t0\ta.txt"`); a
        // rename/copy's counts are their OWN clean token, followed by two
        // separate path tokens — see the module doc for why these two
        // shapes differ. The flags drive which shape each entry gets
        // parsed as.
        let stdout = nul_join(&["1\t0\ta.txt", "2\t0", "old.txt", "new.txt", "-\t-\tb.txt"]);
        let counts = parse_numstat_z(&stdout, &[false, true, false]);
        assert_eq!(counts, vec![(1, 0, false), (2, 0, false), (0, 0, true)]);
    }

    #[test]
    fn parse_numstat_z_matches_a_real_root_commit_single_file_addition() {
        // Byte-for-byte the shape `git diff-tree --numstat -z --no-commit-id
        // -r --root -M <sha>` actually emits for a single-file root commit
        // (verified against a real git 2.55) — regression pin for the
        // "path embedded via tab, not a separate NUL token" bug.
        let stdout = nul_join(&["2\t0\ta.txt"]);
        let counts = parse_numstat_z(&stdout, &[false]);
        assert_eq!(counts, vec![(2, 0, false)]);
    }

    #[test]
    fn merge_combines_status_and_counts_per_file_and_drops_the_rename_score() {
        let name_status = vec![
            ("M".to_string(), None, "a.txt".to_string()),
            (
                "R100".to_string(),
                Some("old.txt".to_string()),
                "new.txt".to_string(),
            ),
        ];
        let numstat = vec![(3, 1, false), (2, 0, false)];
        let merged = merge(name_status, numstat);
        assert_eq!(
            merged,
            vec![
                FileChange {
                    path: "a.txt".to_string(),
                    old_path: None,
                    insertions: 3,
                    deletions: 1,
                    binary: false,
                    status: "M".to_string(),
                },
                FileChange {
                    path: "new.txt".to_string(),
                    old_path: Some("old.txt".to_string()),
                    insertions: 2,
                    deletions: 0,
                    binary: false,
                    status: "R".to_string(),
                },
            ]
        );
    }
}
