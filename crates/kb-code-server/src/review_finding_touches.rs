//! V80-F3 — "lines changed in ps N": for a review finding located at
//! `path:lines` in the patchset it was raised against, does a LATER
//! patchset's diff (own-ps tip -> later-ps tip) touch those same lines?
//! This is EVIDENCE that the author acted near the finding's location — it
//! is never a verdict, never a disposition, and never the word "fixed"
//! anywhere in this module or its wire (root CLAUDE.md #10's
//! surfaced-never-scored posture for memory recall applies here too, by
//! the same reasoning: derived per read, never persisted, never scored).
//!
//! # Where "own ps" comes from
//!
//! A finding's own patchset is NOT a column on `review_findings` — it is
//! the linked `annotations.ps_number` (`AnnotationRow`'s own doc: "the
//! patchset the comment was CREATED against"). Both finding-creation paths
//! (`review_findings::import_findings_route` /
//! `create_manual_finding_route`) always set it, so this module treats a
//! missing annotation (a should-never-happen data-integrity gap) the same
//! way `review_findings::orphaned_resolution` does: an honest empty
//! result, never a guess.
//!
//! # Which side of the diff a finding's `lines` sit on
//!
//! `store::derive_finding_anchor`'s doc: `location_lines` is NEW-side
//! (post-diff) content UNLESS `location_removed` is set, in which case the
//! cited line/file was deleted BY THE FINDING'S OWN PATCHSET and only ever
//! existed on that patchset's OLD side. This module follows the brief's
//! literal instruction and always anchors on the finding's own ps TIP —
//! for a `removed` finding this is a documented approximation (the lines
//! no longer exist at that tip, so a later diff's line numbers cannot line
//! up against them exactly); `removed` findings are a small minority and
//! the alternative (anchoring on `base_sha`, which pulls the finding's OWN
//! patchset's deletion hunk into every later comparison, "touching"
//! trivially every time) is arguably worse. Not fixed in this unit — see
//! the report.
//!
//! # No second `git diff` for the same pair
//!
//! Two caches carry the whole computation: [`RenameCache`] (one
//! `history::diff_files -M` call per DISTINCT `(from_tip, to_tip)` pair,
//! regardless of how many findings/paths ask about it — an import batch
//! commonly creates several findings against the SAME own-ps, so this
//! matters) and a hunk cache keyed on the exact `(from, from_path, to,
//! to_path)` quad. A renamed path is followed via a BLOB-to-blob diff
//! (`diff::diff_blob_pair`) rather than the single-pathspec form, which
//! would otherwise report the file as wholesale deleted (see this
//! module's own tests for why that shortcut is wrong) — see
//! `history/radar.rs`'s own precedent for the `<oid>:<path>` argv shape
//! this relies on (an oid git itself printed, never a caller string, so
//! there is no pathspec position for SEC-17's `--` rule to guard).
//!
//! # The cap
//!
//! [`MAX_TOUCHED_IN_PATCHSETS`] bounds how many LATER patchsets any one
//! finding is checked against. Past it, `touched_in_capped: true` rides
//! the SAME finding object `touched_in` sits on (a per-finding signal, not
//! a review-wide one — the cap is inherently a per-finding computation:
//! "for each finding ... for each patchset later than the finding's own
//! ps") so `compose_finding_view`'s single-finding responses and
//! `list_findings_route`'s batch stay byte-shape-identical, the same law
//! `review_findings::finding_json`'s own doc states.

use crate::diff;
use crate::git::roots::GitCtx;
use crate::git::Revspec;
use crate::history;
use crate::numstat::FileChange;
use crate::review_hunks::{self, DiffHunk};
use crate::store::ReviewPatchsetRow;
use serde::Serialize;
use std::collections::HashMap;

/// How many patchsets LATER than a finding's own ps this module will walk
/// before giving up and naming the response `touched_in_capped: true`.
pub const MAX_TOUCHED_IN_PATCHSETS: usize = 20;

pub const OVERLAP_EXACT: &str = "exact";
pub const OVERLAP_ADJACENT: &str = "adjacent";

/// A later hunk within this many lines of a finding's own range counts as
/// `adjacent` rather than a miss — the same "near enough to be evidence,
/// not proof" spirit `review_turns::MIN_MATCH_BYTES` applies to bytes.
pub const ADJACENT_SLOP: u32 = 3;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct TouchedInEntry {
    pub ps: i64,
    pub hunks: usize,
    pub overlap: &'static str,
}

/// One finding's touched_in INPUT — everything this module needs that
/// isn't already on the `(from_sha, patchsets)` context it's called with.
/// `finding_id` is `review_findings.id` (the row's own primary key, stable
/// and unique per review), the key the caller joins the result back on.
pub struct TouchedInQuery {
    pub finding_id: i64,
    pub own_ps: i64,
    pub path: String,
    pub lines: Vec<i64>,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct TouchedInResult {
    pub entries: Vec<TouchedInEntry>,
    pub capped: bool,
}

/// `(from_tip, to_tip) -> FileChange list`, computed at most once per
/// distinct patchset-tip pair regardless of how many findings share it.
type RenameCache = HashMap<(String, String), Vec<FileChange>>;
/// `(from_sha, from_path, to_sha, to_path) -> parsed hunks`, computed at
/// most once per distinct quad.
type HunkCache = HashMap<(String, String, String, String), Vec<DiffHunk>>;

fn line_bounds(lines: &[i64]) -> Option<(u32, u32)> {
    let mut lo: Option<i64> = None;
    let mut hi: Option<i64> = None;
    for &n in lines {
        lo = Some(lo.map_or(n, |l| l.min(n)));
        hi = Some(hi.map_or(n, |h| h.max(n)));
    }
    match (lo, hi) {
        (Some(l), Some(h)) => Some((l.max(0) as u32, h.max(0) as u32)),
        _ => None,
    }
}

/// `None` (no evidence) | `Some(exact)` | `Some(adjacent)` for one hunk
/// against a finding's `[lo, hi]` line range, both measured on the diff's
/// OLD side (the finding's own ps tip content). A pure zero-width
/// insertion (`old_lines == 0`) can only ever be `adjacent`: it
/// removed/modified no content on the old side, so calling it `exact`
/// would claim a content match this hunk never made.
fn hunk_overlap(lo: u32, hi: u32, hunk: &DiffHunk) -> Option<&'static str> {
    if hunk.old_lines == 0 {
        let point = hunk.old_start;
        // Exactly one side can be positive for a point outside `[lo, hi]`
        // (and both are 0 when it's inside) — `saturating_sub` reads that
        // directly rather than an if/else ladder re-deriving it.
        let gap = lo.saturating_sub(point).max(point.saturating_sub(hi));
        return if gap <= ADJACENT_SLOP {
            Some(OVERLAP_ADJACENT)
        } else {
            None
        };
    }
    let h_lo = hunk.old_start;
    let h_hi = hunk.old_start + hunk.old_lines - 1;
    if h_lo <= hi && lo <= h_hi {
        return Some(OVERLAP_EXACT);
    }
    // Same reasoning as above: the two ranges don't intersect (checked
    // above), so exactly one of these is positive.
    let gap = lo.saturating_sub(h_hi).max(h_lo.saturating_sub(hi));
    if gap <= ADJACENT_SLOP {
        Some(OVERLAP_ADJACENT)
    } else {
        None
    }
}

/// Fold several hunks' verdicts into one per-ps verdict: `exact` beats
/// `adjacent` beats nothing.
fn better(a: Option<&'static str>, b: Option<&'static str>) -> Option<&'static str> {
    if a == Some(OVERLAP_EXACT) || b == Some(OVERLAP_EXACT) {
        Some(OVERLAP_EXACT)
    } else if a == Some(OVERLAP_ADJACENT) || b == Some(OVERLAP_ADJACENT) {
        Some(OVERLAP_ADJACENT)
    } else {
        None
    }
}

/// Where `path` (as it existed at `from_sha`) lives at `to_sha`, per git's
/// OWN rename detection over that exact pair — `None` when the diff
/// between the two trees never mentions `path` at all (the honest "this
/// ps didn't touch it" case, distinct from "touched but returned no
/// hunks"). Memoized in `cache`.
fn resolve_path(
    ctx: &GitCtx,
    cache: &mut RenameCache,
    from_sha: &str,
    to_sha: &str,
    path: &str,
) -> Option<String> {
    let key = (from_sha.to_string(), to_sha.to_string());
    if !cache.contains_key(&key) {
        let range = format!("{from_sha}..{to_sha}");
        let files = ctx
            .read_with_fallback(|root| history::diff_files(root, "diff", &["-M", &range]))
            .unwrap_or_default();
        cache.insert(key.clone(), files);
    }
    let files = cache.get(&key)?;
    for f in files {
        if f.old_path.as_deref() == Some(path) {
            return Some(f.path.clone());
        }
        if f.old_path.is_none() && f.path == path {
            return Some(f.path.clone());
        }
    }
    None
}

/// The hunks of the diff between `(from_sha, from_path)` and `(to_sha,
/// to_path)` — a plain pathspec diff when the path is unchanged (the
/// common case, reuses `diff::diff_file`, already on the SEC-17 allowlist
/// via `diff.rs`), a blob-to-blob diff otherwise (`diff::diff_blob_pair`,
/// this unit's own addition to `diff.rs`). Memoized in `cache`.
fn parsed_hunks(
    ctx: &GitCtx,
    cache: &mut HunkCache,
    from_sha: &str,
    from_path: &str,
    to_sha: &str,
    to_path: &str,
) -> Vec<DiffHunk> {
    let key = (
        from_sha.to_string(),
        from_path.to_string(),
        to_sha.to_string(),
        to_path.to_string(),
    );
    if let Some(hunks) = cache.get(&key) {
        return hunks.clone();
    }
    let text = if from_path == to_path {
        let from = Revspec::trusted(from_sha.to_string());
        let to = Revspec::trusted(to_sha.to_string());
        ctx.read_with_fallback(|root| diff::diff_file(root.git_path(), &from, Some(&to), from_path))
            .unwrap_or_default()
    } else {
        ctx.read_with_fallback(|root| {
            diff::diff_blob_pair(root.git_path(), from_sha, from_path, to_sha, to_path)
        })
        .unwrap_or_default()
    };
    let hunks = review_hunks::parse_unified_diff(&text).hunks;
    cache.insert(key, hunks.clone());
    hunks
}

/// The impure shell: one `TouchedInResult` per query, batched over the
/// SAME two caches (see the module doc). BLOCKING — every caller must run
/// this inside `spawn_blocking`, same rule `diff::diff_file`'s own doc
/// states.
pub fn compute_touched_in(
    ctx: &GitCtx,
    patchsets: &[ReviewPatchsetRow],
    queries: &[TouchedInQuery],
) -> HashMap<i64, TouchedInResult> {
    let mut out: HashMap<i64, TouchedInResult> = HashMap::new();
    let mut rename_cache: RenameCache = HashMap::new();
    let mut hunk_cache: HunkCache = HashMap::new();

    for q in queries {
        let Some((lo, hi)) = line_bounds(&q.lines) else {
            continue;
        };
        let Some(own_row) = patchsets.iter().find(|p| p.ps_number == q.own_ps) else {
            continue;
        };
        let from_sha = own_row.tip_sha.as_str();

        let mut later: Vec<&ReviewPatchsetRow> = patchsets
            .iter()
            .filter(|p| p.ps_number > q.own_ps)
            .collect();
        later.sort_by_key(|p| p.ps_number);
        let capped = later.len() > MAX_TOUCHED_IN_PATCHSETS;
        later.truncate(MAX_TOUCHED_IN_PATCHSETS);

        let mut entries = Vec::new();
        for ps in later {
            let Some(resolved_path) =
                resolve_path(ctx, &mut rename_cache, from_sha, &ps.tip_sha, &q.path)
            else {
                continue;
            };
            let hunks = parsed_hunks(
                ctx,
                &mut hunk_cache,
                from_sha,
                &q.path,
                &ps.tip_sha,
                &resolved_path,
            );
            let mut overlap: Option<&'static str> = None;
            let mut qualifying = 0usize;
            for h in &hunks {
                if let Some(o) = hunk_overlap(lo, hi, h) {
                    qualifying += 1;
                    overlap = better(overlap, Some(o));
                }
            }
            if let Some(o) = overlap {
                entries.push(TouchedInEntry {
                    ps: ps.ps_number,
                    hunks: qualifying,
                    overlap: o,
                });
            }
        }
        out.insert(q.finding_id, TouchedInResult { entries, capped });
    }

    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;
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
        git(dir, &["config", "user.email", "t@example.com"]);
        git(dir, &["config", "user.name", "T"]);
        tmp
    }

    fn ps_row(id: i64, review_id: i64, ps_number: i64, base: &str, tip: &str) -> ReviewPatchsetRow {
        ReviewPatchsetRow {
            id,
            review_id,
            ps_number,
            tip_sha: tip.to_string(),
            base_sha: base.to_string(),
            captured_at: 0,
        }
    }

    #[test]
    fn hunk_overlap_exact_when_ranges_intersect() {
        let h = DiffHunk {
            header: String::new(),
            old_start: 10,
            old_lines: 3,
            new_start: 10,
            new_lines: 3,
            lines: Vec::new(),
        };
        assert_eq!(hunk_overlap(5, 11, &h), Some(OVERLAP_EXACT));
        assert_eq!(hunk_overlap(20, 25, &h), None);
    }

    #[test]
    fn hunk_overlap_adjacent_within_slop_but_not_touching() {
        let h = DiffHunk {
            header: String::new(),
            old_start: 10,
            old_lines: 3, // covers 10..=12
            new_start: 10,
            new_lines: 3,
            lines: Vec::new(),
        };
        // finding at [16,16]; gap to hunk end (12) is 4 lines apart: not
        // adjacent (16 - 12 = 4 > ADJACENT_SLOP).
        assert_eq!(hunk_overlap(16, 16, &h), None);
        // finding at [15,15]; gap is 3 — exactly at the slop floor.
        assert_eq!(hunk_overlap(15, 15, &h), Some(OVERLAP_ADJACENT));
    }

    #[test]
    fn a_zero_width_insertion_is_never_exact() {
        let h = DiffHunk {
            header: String::new(),
            old_start: 10,
            old_lines: 0,
            new_start: 10,
            new_lines: 4,
            lines: Vec::new(),
        };
        assert_eq!(hunk_overlap(10, 10, &h), Some(OVERLAP_ADJACENT));
        assert_eq!(hunk_overlap(9, 9, &h), Some(OVERLAP_ADJACENT));
        assert_eq!(hunk_overlap(50, 50, &h), None);
    }

    #[test]
    fn exact_overlap_end_to_end() {
        let tmp = init_repo();
        let dir = tmp.path();
        std::fs::write(dir.join("a.txt"), "l1\nl2\nl3\nl4\nl5\n").unwrap();
        git(dir, &["add", "a.txt"]);
        git(dir, &["commit", "-q", "-m", "c1"]);
        let ps1_tip = git_out(dir, &["rev-parse", "HEAD"]);
        let ps1_base = ps1_tip.clone();

        std::fs::write(dir.join("a.txt"), "l1\nl2\nl3-changed\nl4\nl5\n").unwrap();
        git(dir, &["commit", "-aq", "-m", "c2"]);
        let ps2_tip = git_out(dir, &["rev-parse", "HEAD"]);

        let patchsets = vec![
            ps_row(1, 1, 1, &ps1_base, &ps1_tip),
            ps_row(2, 1, 2, &ps1_tip, &ps2_tip),
        ];
        let queries = vec![TouchedInQuery {
            finding_id: 42,
            own_ps: 1,
            path: "a.txt".to_string(),
            lines: vec![3],
        }];
        let out = compute_touched_in(
            &GitCtx::work_tree_only(crate::git::roots::WorkTreeRoot::user_clone(dir)),
            &patchsets,
            &queries,
        );
        let result = out.get(&42).expect("finding present");
        assert!(!result.capped);
        assert_eq!(result.entries.len(), 1);
        assert_eq!(result.entries[0].ps, 2);
        assert_eq!(result.entries[0].overlap, OVERLAP_EXACT);
        assert_eq!(result.entries[0].hunks, 1);
    }

    #[test]
    fn adjacent_overlap_end_to_end() {
        let tmp = init_repo();
        let dir = tmp.path();
        let lines: Vec<String> = (1..=20).map(|n| format!("l{n}")).collect();
        std::fs::write(dir.join("a.txt"), format!("{}\n", lines.join("\n"))).unwrap();
        git(dir, &["add", "a.txt"]);
        git(dir, &["commit", "-q", "-m", "c1"]);
        let ps1_tip = git_out(dir, &["rev-parse", "HEAD"]);

        // Change line 10 only. `git diff -U3` pads the hunk with 3 lines of
        // CONTEXT each side, so its old-side span is [7,13] — a finding
        // cited INSIDE that padded span (e.g. line 12) would read `exact`
        // (the hunk's whole old-side range, this module's literal
        // contract). Line 15 sits just past the padded edge: gap to h_hi
        // (13) is 2, within ADJACENT_SLOP but outside the hunk itself —
        // exactly the case `adjacent` exists for.
        let mut edited = lines.clone();
        edited[9] = "l10-changed".to_string();
        std::fs::write(dir.join("a.txt"), format!("{}\n", edited.join("\n"))).unwrap();
        git(dir, &["commit", "-aq", "-m", "c2"]);
        let ps2_tip = git_out(dir, &["rev-parse", "HEAD"]);

        let patchsets = vec![
            ps_row(1, 1, 1, &ps1_tip, &ps1_tip),
            ps_row(2, 1, 2, &ps1_tip, &ps2_tip),
        ];
        let queries = vec![TouchedInQuery {
            finding_id: 7,
            own_ps: 1,
            path: "a.txt".to_string(),
            lines: vec![15],
        }];
        let out = compute_touched_in(
            &GitCtx::work_tree_only(crate::git::roots::WorkTreeRoot::user_clone(dir)),
            &patchsets,
            &queries,
        );
        let result = out.get(&7).expect("finding present");
        assert_eq!(result.entries.len(), 1);
        assert_eq!(result.entries[0].overlap, OVERLAP_ADJACENT);
    }

    #[test]
    fn no_overlap_when_the_later_diff_never_comes_near() {
        let tmp = init_repo();
        let dir = tmp.path();
        let lines: Vec<String> = (1..=40).map(|n| format!("l{n}")).collect();
        std::fs::write(dir.join("a.txt"), format!("{}\n", lines.join("\n"))).unwrap();
        git(dir, &["add", "a.txt"]);
        git(dir, &["commit", "-q", "-m", "c1"]);
        let ps1_tip = git_out(dir, &["rev-parse", "HEAD"]);

        let mut edited = lines.clone();
        edited[35] = "l36-changed".to_string();
        std::fs::write(dir.join("a.txt"), format!("{}\n", edited.join("\n"))).unwrap();
        git(dir, &["commit", "-aq", "-m", "c2"]);
        let ps2_tip = git_out(dir, &["rev-parse", "HEAD"]);

        let patchsets = vec![
            ps_row(1, 1, 1, &ps1_tip, &ps1_tip),
            ps_row(2, 1, 2, &ps1_tip, &ps2_tip),
        ];
        let queries = vec![TouchedInQuery {
            finding_id: 9,
            own_ps: 1,
            path: "a.txt".to_string(),
            lines: vec![3],
        }];
        let out = compute_touched_in(
            &GitCtx::work_tree_only(crate::git::roots::WorkTreeRoot::user_clone(dir)),
            &patchsets,
            &queries,
        );
        let result = out.get(&9).expect("finding present");
        assert!(result.entries.is_empty());
        assert!(!result.capped);
    }

    #[test]
    fn a_renamed_file_is_followed_to_its_new_path() {
        let tmp = init_repo();
        let dir = tmp.path();
        std::fs::write(dir.join("old.txt"), "l1\nl2\nl3\nl4\nl5\n").unwrap();
        git(dir, &["add", "old.txt"]);
        git(dir, &["commit", "-q", "-m", "c1"]);
        let ps1_tip = git_out(dir, &["rev-parse", "HEAD"]);

        git(dir, &["mv", "old.txt", "new.txt"]);
        std::fs::write(dir.join("new.txt"), "l1\nl2\nl3-changed\nl4\nl5\n").unwrap();
        git(dir, &["add", "-A"]);
        git(dir, &["commit", "-q", "-m", "c2 rename+edit"]);
        let ps2_tip = git_out(dir, &["rev-parse", "HEAD"]);

        let patchsets = vec![
            ps_row(1, 1, 1, &ps1_tip, &ps1_tip),
            ps_row(2, 1, 2, &ps1_tip, &ps2_tip),
        ];
        // The finding was raised against `old.txt` (its own ps's path) —
        // both endpoints of the rename matter: the QUERY names the OLD
        // path, the diff must resolve through to the NEW one.
        let queries = vec![TouchedInQuery {
            finding_id: 3,
            own_ps: 1,
            path: "old.txt".to_string(),
            lines: vec![3],
        }];
        let out = compute_touched_in(
            &GitCtx::work_tree_only(crate::git::roots::WorkTreeRoot::user_clone(dir)),
            &patchsets,
            &queries,
        );
        let result = out.get(&3).expect("finding present");
        assert_eq!(result.entries.len(), 1);
        assert_eq!(result.entries[0].ps, 2);
        assert_eq!(result.entries[0].overlap, OVERLAP_EXACT);
    }

    #[test]
    fn a_pure_rename_with_no_content_change_reports_no_hunks() {
        // Sanity check on the rename machinery itself: renaming a file
        // WITHOUT touching its content must not manufacture a spurious
        // "touched" signal (which the naive single-pathspec shortcut this
        // module's doc warns against WOULD do — it would see the old path
        // vanish and report a full-file deletion).
        let tmp = init_repo();
        let dir = tmp.path();
        std::fs::write(dir.join("old.txt"), "l1\nl2\nl3\nl4\nl5\n").unwrap();
        git(dir, &["add", "old.txt"]);
        git(dir, &["commit", "-q", "-m", "c1"]);
        let ps1_tip = git_out(dir, &["rev-parse", "HEAD"]);

        git(dir, &["mv", "old.txt", "new.txt"]);
        git(dir, &["commit", "-q", "-m", "c2 pure rename"]);
        let ps2_tip = git_out(dir, &["rev-parse", "HEAD"]);

        let patchsets = vec![
            ps_row(1, 1, 1, &ps1_tip, &ps1_tip),
            ps_row(2, 1, 2, &ps1_tip, &ps2_tip),
        ];
        let queries = vec![TouchedInQuery {
            finding_id: 5,
            own_ps: 1,
            path: "old.txt".to_string(),
            lines: vec![3],
        }];
        let out = compute_touched_in(
            &GitCtx::work_tree_only(crate::git::roots::WorkTreeRoot::user_clone(dir)),
            &patchsets,
            &queries,
        );
        let result = out.get(&5).expect("finding present");
        assert!(
            result.entries.is_empty(),
            "a content-identical rename must report no hunks, got {:?}",
            result.entries
        );
    }

    #[test]
    fn the_cap_truncates_later_patchsets_and_says_so() {
        let tmp = init_repo();
        let dir = tmp.path();
        std::fs::write(dir.join("a.txt"), "l1\nl2\nl3\nl4\nl5\n").unwrap();
        git(dir, &["add", "a.txt"]);
        git(dir, &["commit", "-q", "-m", "c1"]);
        let ps1_tip = git_out(dir, &["rev-parse", "HEAD"]);

        let mut patchsets = vec![ps_row(1, 1, 1, &ps1_tip, &ps1_tip)];
        let mut prev_tip = ps1_tip.clone();
        // 22 later patchsets — one over MAX_TOUCHED_IN_PATCHSETS (20) plus
        // one, so the cap must fire and at least one qualifying ps must be
        // excluded from the walk.
        for n in 2..=23 {
            std::fs::write(dir.join("a.txt"), format!("l1\nl2\nl3-v{n}\nl4\nl5\n")).unwrap();
            git(dir, &["commit", "-aq", "-m", &format!("c{n}")]);
            let tip = git_out(dir, &["rev-parse", "HEAD"]);
            patchsets.push(ps_row(n, 1, n, &prev_tip, &tip));
            prev_tip = tip;
        }
        assert_eq!(patchsets.len(), 23);

        let queries = vec![TouchedInQuery {
            finding_id: 1,
            own_ps: 1,
            path: "a.txt".to_string(),
            lines: vec![3],
        }];
        let out = compute_touched_in(
            &GitCtx::work_tree_only(crate::git::roots::WorkTreeRoot::user_clone(dir)),
            &patchsets,
            &queries,
        );
        let result = out.get(&1).expect("finding present");
        assert!(result.capped, "22 later patchsets must trip the cap");
        assert!(result.entries.len() <= MAX_TOUCHED_IN_PATCHSETS);
    }

    #[test]
    fn findings_with_no_lines_are_never_queried() {
        // `line_bounds` on an empty slice — the caller's own responsibility
        // not to build a query for a whole_file/no-lines finding, but this
        // pins the degrade if one slips through anyway.
        assert_eq!(line_bounds(&[]), None);
    }
}
