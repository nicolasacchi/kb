//! The conflict radar (V75-M3, design D15) — "which of these branches
//! would collide with `main` if you merged them today?", answered offline,
//! before any PR exists.
//!
//! # Relationship to `merge_check`
//!
//! [`super::merge_check`] answers the same question for ONE pair and is
//! the mechanism this module fans out: `git merge-tree --write-tree`
//! against a per-request scratch object directory (SEC-15,
//! [`super::scratch::ScratchOdb`]), never the browsed repo's own ODB. What
//! is new here is the fan-out, and therefore the budget:
//!
//! * a HARD pair cap ([`MAX_PAIRS`]) — the response says `computed` of
//!   `candidates`, never silently truncating to look complete;
//! * ONE scratch ODB per pair, dropped before the next runs (the Drop
//!   guard, plus the boot sweep, is what keeps a panic mid-fan-out from
//!   leaving 40 directories behind);
//! * the caller holds a `git_fanout` permit per child, exactly as
//!   `merge_check_route` does.
//!
//! # A richer parse than `--name-only`
//!
//! `merge_check` passes `--name-only`, which collapses the conflicted-file
//! section to bare paths. The radar wants the KIND of each collision, so
//! it omits that flag and reads the stage lines git emits instead:
//!
//! ```text
//! <tree oid>
//! 100644 <blob> 1\tf.txt      ← stage 1 = merge base
//! 100644 <blob> 2\tf.txt      ← stage 2 = "ours"
//! 100644 <blob> 3\tf.txt      ← stage 3 = "theirs"
//! <blank line>
//! CONFLICT (content): Merge conflict in f.txt
//! ```
//!
//! The kind is derived from the STAGE SET, not from the informational
//! message: the messages are prose whose per-kind shape differs (a
//! `modify/delete` line does not put the path in a fixed position), while
//! the stage set is structured and closed. [`ConflictKind`] is therefore
//! this module's OWN small vocabulary, derived deterministically, rather
//! than a re-scrape of git's English.
//!
//! # Hunk counts are measured, never estimated
//!
//! For a content conflict, `--write-tree`'s result tree contains the
//! CONFLICT-MARKED blob. `git show <tree>:<path>` (with the same scratch
//! ODB env) reads it back and the `<<<<<<<` markers ARE the hunks. That is
//! one extra subprocess per conflicted path, so it runs under its own
//! per-request budget ([`MAX_HUNK_PROBES`]); past the budget `hunks` is
//! `null` and `hunk_budget_exhausted` is `true` — never a guessed number
//! and never a silently-dropped field. A non-content conflict has no
//! markers to count and reports `hunks: 0` with its own kind, which is the
//! honest answer rather than a missing one.
//!
//! # Read-only mounts
//!
//! The browsed repo needs no write access at all — that is precisely what
//! SEC-15's scratch redirection bought. What the radar DOES need is a
//! writable SCRATCH ROOT (the daemon's own state dir). When that is
//! unavailable the refusal is TYPED
//! ([`super::HistoryError::ScratchUnwritable`] →
//! `urn:kb:errors:scratch-unwritable`) and names the directory, rather
//! than surfacing as a generic 500 — and `merge_check` inherits the same
//! typed refusal, since both go through `ScratchOdb::create`.

use super::scratch::ScratchOdb;
use super::{HistoryError, Result};
use crate::git::Revspec;
use std::collections::BTreeMap;
use std::path::Path;
use std::process::Command;

/// Wire schema name.
pub const SCHEMA: &str = "branch-conflicts/1";

/// HARD ceiling on merge-tree pairs per request. A radar over a repo with
/// 300 branches computes 40 and SAYS so.
pub const MAX_PAIRS: usize = 40;

/// Default when `?limit=` is absent.
pub const DEFAULT_PAIRS: usize = 20;

/// Per-request budget for `git show <tree>:<path>` hunk probes.
pub const MAX_HUNK_PROBES: usize = 60;

/// A conflict's shape, derived from the stage set — see the module doc.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum ConflictKind {
    /// Stages 1+2+3: both sides changed the file.
    BothModified,
    /// Stages 1+3: deleted on the left, modified on the right.
    ModifyDelete,
    /// Stages 1+2: modified on the left, deleted on the right.
    DeleteModify,
    /// Stages 2+3 with no base: both sides added the path independently.
    AddAdd,
    /// Any other stage set — reported as such rather than forced into one
    /// of the four above.
    Other,
}

impl ConflictKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::BothModified => "both-modified",
            Self::ModifyDelete => "modify-delete",
            Self::DeleteModify => "delete-modify",
            Self::AddAdd => "add-add",
            Self::Other => "other",
        }
    }

    /// Only a content conflict has conflict markers to count.
    pub fn has_markers(self) -> bool {
        matches!(self, Self::BothModified)
    }
}

/// Derive the kind from the sorted, de-duplicated stage set.
pub fn kind_for_stages(stages: &[u8]) -> ConflictKind {
    match stages {
        [1, 2, 3] => ConflictKind::BothModified,
        [1, 3] => ConflictKind::ModifyDelete,
        [1, 2] => ConflictKind::DeleteModify,
        [2, 3] => ConflictKind::AddAdd,
        _ => ConflictKind::Other,
    }
}

/// One conflicted path in one pair.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct ConflictPath {
    pub path: String,
    pub kind: ConflictKind,
    /// The raw stage numbers git reported, ascending — the evidence the
    /// `kind` was derived from.
    pub stages: Vec<u8>,
    /// `<<<<<<<` markers in the merged blob. `None` = not measured (budget
    /// exhausted, or the blob could not be read) — never a guessed count.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub hunks: Option<u32>,
}

/// One candidate branch's result.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct RadarRow {
    /// The branch's display name.
    pub branch: String,
    /// The full ref the merge-tree actually ran against.
    pub full_ref: String,
    pub tip_sha: String,
    pub clean: bool,
    pub conflicts: Vec<ConflictPath>,
    /// Set instead of a result when THIS pair failed (an unresolvable ref,
    /// a git error). The rest of the fan-out still reports — one bad pair
    /// never 500s the radar.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

/// The fan-out's honest budget report.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct RadarBudget {
    /// Pairs actually merge-tree'd.
    pub computed: usize,
    /// Pairs that were eligible.
    pub candidates: usize,
    pub pair_cap: usize,
    pub hunk_probes_used: usize,
    pub hunk_probe_cap: usize,
    pub hunk_budget_exhausted: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct Radar {
    pub against: String,
    pub against_sha: String,
    pub rows: Vec<RadarRow>,
    pub budget: RadarBudget,
}

/// One git child's `(exit status, stdout, stderr)`. Its own type rather
/// than a tuple because both callers below read all three fields by name
/// and a `(i32, Vec<u8>, Vec<u8>)` reads identically whichever way round
/// the two byte vectors go.
struct MergeTreeRun {
    status: i32,
    stdout: Vec<u8>,
    stderr: Vec<u8>,
}

/// SEC-15 — every git child here runs with the scratch ODB primary and the
/// repo's real objects as a read-only alternate. Kept as one helper so the
/// two env vars can never be set on one call and forgotten on the other.
fn git_in_scratch(repo_root: &Path, scratch: &ScratchOdb, args: &[&str]) -> Result<MergeTreeRun> {
    let output = Command::new("git")
        .arg("-C")
        .arg(repo_root)
        .env("GIT_OBJECT_DIRECTORY", scratch.dir())
        .env("GIT_ALTERNATE_OBJECT_DIRECTORIES", scratch.alternates())
        .args(args)
        .output()
        .map_err(HistoryError::Spawn)?;
    Ok(MergeTreeRun {
        status: output.status.code().unwrap_or(-1),
        stdout: output.stdout,
        stderr: output.stderr,
    })
}

fn looks_like_oid(line: &str) -> bool {
    let l = line.trim();
    !l.is_empty() && l.len() <= 64 && l.bytes().all(|b| b.is_ascii_hexdigit())
}

/// `(result tree oid, conflicted paths)`.
///
/// Exit 1 means BOTH "conflicts found" and "unresolvable ref" — the same
/// ambiguity `merge_check`'s module doc records — and is disambiguated the
/// same way: a genuine merge always writes its tree oid as stdout's first
/// line, an unresolvable ref writes nothing to stdout.
pub fn parse_merge_tree(
    status: i32,
    stdout: &[u8],
    stderr: &[u8],
) -> Result<(String, Vec<ConflictPath>)> {
    let text = String::from_utf8_lossy(stdout);
    let mut lines = text.lines();
    let first = lines.next().unwrap_or("").trim().to_string();
    if !looks_like_oid(&first) || (status != 0 && status != 1) {
        return Err(HistoryError::GitFailed {
            status,
            stderr: String::from_utf8_lossy(stderr).trim().to_string(),
        });
    }
    if status == 0 {
        return Ok((first, Vec::new()));
    }
    // Stage lines until the first blank line; everything after is git's
    // own informational prose, which this parser deliberately ignores.
    let mut stages: BTreeMap<String, Vec<u8>> = BTreeMap::new();
    let mut order: Vec<String> = Vec::new();
    for line in lines.take_while(|l| !l.trim().is_empty()) {
        let Some((info, path)) = line.split_once('\t') else {
            continue;
        };
        let mut f = info.split_whitespace();
        let (_mode, _oid, stage) = (f.next(), f.next(), f.next());
        let Some(stage) = stage.and_then(|s| s.parse::<u8>().ok()) else {
            continue;
        };
        let entry = stages.entry(path.to_string()).or_insert_with(|| {
            order.push(path.to_string());
            Vec::new()
        });
        if !entry.contains(&stage) {
            entry.push(stage);
        }
    }
    let mut out = Vec::with_capacity(order.len());
    for path in order {
        let mut s = stages.remove(&path).unwrap_or_default();
        s.sort_unstable();
        out.push(ConflictPath {
            kind: kind_for_stages(&s),
            path,
            stages: s,
            hunks: None,
        });
    }
    Ok((first, out))
}

/// Count `<<<<<<<` conflict markers at line starts.
pub fn count_markers(bytes: &[u8]) -> u32 {
    String::from_utf8_lossy(bytes)
        .lines()
        .filter(|l| l.starts_with("<<<<<<<"))
        .count() as u32
}

/// One candidate for the fan-out.
#[derive(Debug, Clone)]
pub struct Candidate {
    pub branch: String,
    pub full_ref: String,
    pub tip_sha: String,
}

/// Run the radar. `against` is the already-validated target ref;
/// `candidates` are already capped by the caller to at most [`MAX_PAIRS`]
/// (`candidate_total` is what the cap was applied to, and is what the
/// budget reports).
///
/// Blocking: the caller runs this inside `spawn_blocking` while holding a
/// `git_fanout` permit.
pub fn radar(
    repo_root: &Path,
    scratch_root: &Path,
    against: &Revspec,
    against_sha: &str,
    candidates: &[Candidate],
    candidate_total: usize,
) -> Result<Radar> {
    let mut rows = Vec::with_capacity(candidates.len());
    let mut hunk_probes_used = 0usize;
    let mut hunk_budget_exhausted = false;

    for c in candidates {
        // One scratch ODB per pair — created (and dropped) inside the loop
        // so a 40-pair fan-out never holds 40 directories open at once.
        // A creation failure is the read-only-mount refusal and aborts the
        // WHOLE request: it is a property of the daemon's state dir, not
        // of this pair, so retrying 39 more times would just be 39 more
        // identical failures.
        let scratch = ScratchOdb::create(repo_root, scratch_root)?;
        let Ok(head) = Revspec::parse(&c.full_ref) else {
            rows.push(RadarRow {
                branch: c.branch.clone(),
                full_ref: c.full_ref.clone(),
                tip_sha: c.tip_sha.clone(),
                clean: false,
                conflicts: Vec::new(),
                error: Some("ref failed validation".to_string()),
            });
            continue;
        };
        let run = git_in_scratch(
            repo_root,
            &scratch,
            &[
                "merge-tree",
                "--write-tree",
                against.as_str(),
                head.as_str(),
            ],
        )?;
        match parse_merge_tree(run.status, &run.stdout, &run.stderr) {
            Ok((tree, mut conflicts)) => {
                for cp in conflicts.iter_mut() {
                    if !cp.kind.has_markers() {
                        // A tree-level conflict has no markers to count —
                        // 0 is the measured answer here, not a guess.
                        cp.hunks = Some(0);
                        continue;
                    }
                    if hunk_probes_used >= MAX_HUNK_PROBES {
                        hunk_budget_exhausted = true;
                        continue;
                    }
                    hunk_probes_used += 1;
                    let spec = format!("{tree}:{}", cp.path);
                    if let Ok(show) = git_in_scratch(repo_root, &scratch, &["show", &spec]) {
                        if show.status == 0 {
                            cp.hunks = Some(count_markers(&show.stdout));
                        }
                    }
                }
                rows.push(RadarRow {
                    branch: c.branch.clone(),
                    full_ref: c.full_ref.clone(),
                    tip_sha: c.tip_sha.clone(),
                    clean: conflicts.is_empty(),
                    conflicts,
                    error: None,
                });
            }
            // One unresolvable pair is reported as a row with an error, not
            // as a failure of the whole radar (the drop-on-error fan-out
            // posture root CLAUDE.md #28 records).
            Err(e) => rows.push(RadarRow {
                branch: c.branch.clone(),
                full_ref: c.full_ref.clone(),
                tip_sha: c.tip_sha.clone(),
                clean: false,
                conflicts: Vec::new(),
                error: Some(e.to_string()),
            }),
        }
    }

    Ok(Radar {
        against: against.as_str().to_string(),
        against_sha: against_sha.to_string(),
        budget: RadarBudget {
            computed: rows.len(),
            candidates: candidate_total,
            pair_cap: MAX_PAIRS,
            hunk_probes_used,
            hunk_probe_cap: MAX_HUNK_PROBES,
            hunk_budget_exhausted,
        },
        rows,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stage_sets_map_onto_the_closed_kind_vocabulary() {
        assert_eq!(kind_for_stages(&[1, 2, 3]), ConflictKind::BothModified);
        assert_eq!(kind_for_stages(&[1, 3]), ConflictKind::ModifyDelete);
        assert_eq!(kind_for_stages(&[1, 2]), ConflictKind::DeleteModify);
        assert_eq!(kind_for_stages(&[2, 3]), ConflictKind::AddAdd);
        assert_eq!(kind_for_stages(&[2]), ConflictKind::Other);
        assert_eq!(kind_for_stages(&[]), ConflictKind::Other);
    }

    /// Captured from `git merge-tree --write-tree a b` on git 2.55 for a
    /// content conflict PLUS a modify/delete — the exact bytes the module
    /// doc quotes.
    const CONFLICTED: &[u8] = b"12b13e8e90675493a2fc4456f1b4d1b9557aecf4\n\
100644 83db48f84ec878fbfb30b46d16630e944e34f205 1\tf.txt\n\
100644 b5fd75c02c7bd3646d34f9c096142a2e574ebd3b 2\tf.txt\n\
100644 7680da39011605833c112a48803023ce831828ce 3\tf.txt\n\
100644 587be6b4c3f93f93c489c0111bba5596147a26cb 1\tg.txt\n\
100644 b77b4eb1d946f923f61785536da9ca5af6909f06 3\tg.txt\n\
\n\
Auto-merging f.txt\n\
CONFLICT (content): Merge conflict in f.txt\n\
CONFLICT (modify/delete): g.txt deleted in a and modified in b.\n";

    #[test]
    fn a_conflicted_run_parses_paths_stages_and_kinds() {
        let (tree, conflicts) = parse_merge_tree(1, CONFLICTED, b"").unwrap();
        assert_eq!(tree, "12b13e8e90675493a2fc4456f1b4d1b9557aecf4");
        assert_eq!(conflicts.len(), 2);
        assert_eq!(conflicts[0].path, "f.txt");
        assert_eq!(conflicts[0].stages, vec![1, 2, 3]);
        assert_eq!(conflicts[0].kind, ConflictKind::BothModified);
        assert!(conflicts[0].kind.has_markers());
        assert_eq!(conflicts[1].path, "g.txt");
        assert_eq!(conflicts[1].stages, vec![1, 3]);
        assert_eq!(conflicts[1].kind, ConflictKind::ModifyDelete);
        assert!(!conflicts[1].kind.has_markers());
    }

    #[test]
    fn the_informational_prose_after_the_blank_line_is_never_parsed_as_a_path() {
        let (_tree, conflicts) = parse_merge_tree(1, CONFLICTED, b"").unwrap();
        assert!(
            conflicts
                .iter()
                .all(|c| c.path == "f.txt" || c.path == "g.txt"),
            "a CONFLICT (...) message line must not become a path: {conflicts:?}"
        );
    }

    #[test]
    fn a_clean_run_is_an_oid_and_no_conflicts() {
        let (tree, conflicts) =
            parse_merge_tree(0, b"09dbf22b812062fac5add81298938133552c48ee\n", b"").unwrap();
        assert_eq!(tree, "09dbf22b812062fac5add81298938133552c48ee");
        assert!(conflicts.is_empty());
    }

    #[test]
    fn an_unresolvable_ref_is_a_git_failure_not_a_clean_merge() {
        // Exit 1 with NOTHING on stdout — the shape a typo'd branch name
        // produces (`merge_check`'s module doc records the same finding).
        let err = parse_merge_tree(1, b"", b"fatal: not something we can merge").unwrap_err();
        assert!(matches!(err, HistoryError::GitFailed { .. }), "{err:?}");
        // And a nonsense exit code is never silently "clean" either.
        assert!(parse_merge_tree(128, b"", b"boom").is_err());
    }

    #[test]
    fn conflict_markers_are_counted_at_line_starts_only() {
        let blob =
            b"a\n<<<<<<< ours\nx\n=======\ny\n>>>>>>> theirs\nb\n  <<<<<<< indented\n<<<<<<< two\n";
        assert_eq!(count_markers(blob), 2);
        assert_eq!(count_markers(b""), 0);
    }
}
