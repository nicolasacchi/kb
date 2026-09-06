//! Sub-step (b) — the per-repo gate-machine (ADR-3: "distrust file events,
//! reconcile-don't-replay on git operations").
//!
//! State lives in [`RepoGate`]; the pure classification helpers below
//! (`classify_git_dir_path`, `markers_present`) decide what a raw event
//! under a repo's git-dir domain MEANS, without touching git or the
//! filesystem watch itself — kept separate from `mod.rs`'s orchestration so
//! both halves are unit-testable in isolation (this file needs no real
//! `notify` events or `GitRepo`, just paths and booleans).

use std::collections::HashSet;
use std::path::{Path, PathBuf};

/// The five marker paths (direct children of a repo's OWN `git_dir()`)
/// whose mere existence means "a git operation is in flight" — ADR-3.
/// `index.lock` is deliberately NOT one of these: it is filtered
/// unconditionally below, never treated as an op-state signal (`git add`
/// cycles it on every invocation and it is gone before any consumer could
/// observe a meaningful in-between state).
pub const MARKER_NAMES: &[&str] = &[
    "rebase-merge",
    "rebase-apply",
    "MERGE_HEAD",
    "CHERRY_PICK_HEAD",
    "BISECT_LOG",
];

/// What a git-dir-domain path means to ONE specific repo's gate — computed
/// against that repo's own `git_dir`/`common_dir`, never by basename alone.
/// This repo-specificity is load-bearing: in a linked-worktree registration
/// (matrix case 5), `main`'s `git_dir` IS `linked`'s `common_dir`, so a raw
/// event under that shared physical directory gets routed to BOTH repos'
/// gates by `mod.rs` (a cheap over-approximation) — it is `classify_git_dir_path`
/// re-checking the path against each repo's OWN paths (not a basename set
/// membership test) that keeps `main`'s rebase from ever registering as a
/// marker for `linked`, and vice versa.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GitDirSignal {
    /// `index.lock`, direct child of `git_dir` — always a no-op.
    IndexLockNoise,
    /// One of [`MARKER_NAMES`], direct child of `git_dir` — re-stat all
    /// markers to decide the current op-state.
    Marker,
    /// `HEAD` (direct child of `git_dir`), `logs/HEAD`, or the shared
    /// `packed-refs` — "HEAD may have moved for this repo, go look."
    HeadCandidate,
    /// Anything else under `git_dir`/`common_dir` (e.g. `index` itself,
    /// `COMMIT_EDITMSG`, `FETCH_HEAD`, `ORIG_HEAD`, ref leaf files) — this
    /// watcher doesn't act on it.
    Ignored,
}

/// Classify `path` against ONE repo's `git_dir`/`common_dir`. See
/// [`GitDirSignal`]'s doc for why repo-specificity (not a basename-only
/// check) matters.
pub fn classify_git_dir_path(git_dir: &Path, common_dir: &Path, path: &Path) -> GitDirSignal {
    if path.parent() == Some(git_dir) {
        if let Some(basename) = path.file_name().and_then(|s| s.to_str()) {
            if basename == "index.lock" {
                return GitDirSignal::IndexLockNoise;
            }
            if MARKER_NAMES.contains(&basename) {
                return GitDirSignal::Marker;
            }
            if basename == "HEAD" {
                return GitDirSignal::HeadCandidate;
            }
        }
    }
    if path == git_dir.join("logs").join("HEAD") || path == common_dir.join("packed-refs") {
        return GitDirSignal::HeadCandidate;
    }
    GitDirSignal::Ignored
}

/// Re-stat every [`MARKER_NAMES`] entry under `git_dir`. Never inferred from
/// an event's Create/Remove kind alone — a debounced flush can coalesce a
/// marker's whole lifetime into one event, or (backend-dependent) deliver
/// pieces out of order, so the filesystem is always the source of truth.
pub fn markers_present(git_dir: &Path) -> bool {
    MARKER_NAMES.iter().any(|m| git_dir.join(m).exists())
}

/// Which of the five [`MARKER_NAMES`] operations (if any) is currently in
/// flight — the RICHER classification `GET /api/repo-state`
/// (`crate::repo_state`) needs, built on the exact same marker list
/// `markers_present` already checks presence of (never a second, drifting
/// copy of that list). `rename_all = "kebab-case"` gives the wire strings
/// the phase brief specifies directly (`"none"`, `"rebase"`, `"merge"`,
/// `"cherry-pick"`, `"bisect"`) with no separate `as_str` mapping to keep
/// in sync.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum RepoOp {
    None,
    Rebase,
    Merge,
    CherryPick,
    Bisect,
}

/// Op-specific detail read from the SAME marker file(s) that decided
/// [`RepoOp`] — every field independently optional since which ones apply
/// depends entirely on the op (`rebase` populates `step`/`total`;
/// `merge`/`cherry-pick` populate `head_sha`; `bisect`/`none` populate
/// nothing, serializing to `{}`).
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize)]
pub struct OpDetail {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub step: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub total: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub head_sha: Option<String>,
}

/// A single-line marker file's trimmed text, or `None` on any read failure
/// (missing/unreadable/empty) — never a hard error: a marker file
/// vanishing between the existence check just below and this read simply
/// means the operation it named finished mid-request, which the NEXT
/// request's fresh [`detect_op`] call reflects correctly either way.
fn read_marker(path: &Path) -> Option<String> {
    std::fs::read_to_string(path)
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

fn parse_u32(s: Option<String>) -> Option<u32> {
    s.and_then(|s| s.trim().parse().ok())
}

/// Classify the CURRENT git operation (if any) in flight under `git_dir`,
/// with op-specific detail read from that SAME marker file — the shared
/// helper `crate::repo_state` (`GET /api/repo-state`) calls rather than
/// re-deriving [`MARKER_NAMES`] or re-implementing the existence checks
/// [`markers_present`] already does. Ordering mirrors `MARKER_NAMES`: a
/// rebase directory takes priority over MERGE_HEAD/CHERRY_PICK_HEAD/
/// BISECT_LOG — git itself refuses to start a merge/cherry-pick/bisect
/// while a rebase is mid-flight (and vice versa), so in practice these are
/// mutually exclusive; the fixed order just keeps this deterministic even
/// against a hand-corrupted `.git` dir with more than one marker present.
pub fn detect_op(git_dir: &Path) -> (RepoOp, OpDetail) {
    let rebase_merge = git_dir.join("rebase-merge");
    if rebase_merge.is_dir() {
        let step = parse_u32(read_marker(&rebase_merge.join("msgnum")));
        let total = parse_u32(read_marker(&rebase_merge.join("end")));
        return (
            RepoOp::Rebase,
            OpDetail {
                step,
                total,
                head_sha: None,
            },
        );
    }
    let rebase_apply = git_dir.join("rebase-apply");
    if rebase_apply.is_dir() {
        let step = parse_u32(read_marker(&rebase_apply.join("next")));
        let total = parse_u32(read_marker(&rebase_apply.join("last")));
        return (
            RepoOp::Rebase,
            OpDetail {
                step,
                total,
                head_sha: None,
            },
        );
    }
    let merge_head = git_dir.join("MERGE_HEAD");
    if merge_head.is_file() {
        let head_sha = read_marker(&merge_head);
        return (
            RepoOp::Merge,
            OpDetail {
                head_sha,
                ..Default::default()
            },
        );
    }
    let cherry_pick_head = git_dir.join("CHERRY_PICK_HEAD");
    if cherry_pick_head.is_file() {
        let head_sha = read_marker(&cherry_pick_head);
        return (
            RepoOp::CherryPick,
            OpDetail {
                head_sha,
                ..Default::default()
            },
        );
    }
    let bisect_log = git_dir.join("BISECT_LOG");
    if bisect_log.is_file() {
        return (RepoOp::Bisect, OpDetail::default());
    }
    (RepoOp::None, OpDetail::default())
}

/// Per-repo mutable gate state. Fields are `pub(super)` — `mod.rs` drives
/// the state machine directly (see its `handle_git_dir_event`/
/// `process_flush`); this struct only owns the data, not the transition
/// policy, so the policy stays readable as one linear function instead of
/// being smeared across `note_*`-style setters.
#[derive(Debug, Default)]
pub struct RepoGate {
    /// `true` while any [`MARKER_NAMES`] path exists under this repo's
    /// `git_dir` — working-tree events are held, never emitted, while this
    /// is `true`.
    pub(super) suspended: bool,
    /// The last HEAD this gate confirmed (`None` only before the startup
    /// reconcile has run once, or while the repo's HEAD is still unborn).
    pub(super) last_head: Option<gix::ObjectId>,
    /// Working-tree paths touched since the last reconcile — deduped by
    /// path (a `HashSet`, not a `Vec`: "write 10 files in a burst → each
    /// reported once" applies here too, not just to the fast emit path).
    /// Drained (never replayed) by a reconcile.
    pub(super) held: HashSet<PathBuf>,
}

impl RepoGate {
    pub fn new() -> Self {
        Self::default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn git_dir_with(tmp: &Path, children: &[&str]) -> PathBuf {
        let git_dir = tmp.join(".git");
        std::fs::create_dir_all(&git_dir).unwrap();
        for c in children {
            let p = git_dir.join(c);
            if c.ends_with("-merge") || c.ends_with("-apply") {
                std::fs::create_dir_all(p).unwrap();
            } else {
                std::fs::write(p, b"x").unwrap();
            }
        }
        git_dir
    }

    #[test]
    fn markers_present_detects_each_marker_name() {
        let tmp = tempfile::tempdir().unwrap();
        let git_dir = tmp.path().join(".git");
        std::fs::create_dir_all(&git_dir).unwrap();
        assert!(!markers_present(&git_dir));
        for m in MARKER_NAMES {
            let clean = tempfile::tempdir().unwrap();
            let gd = git_dir_with(clean.path(), &[m]);
            assert!(markers_present(&gd), "expected {m} to register as present");
        }
    }

    #[test]
    fn index_lock_never_a_marker() {
        let tmp = tempfile::tempdir().unwrap();
        let git_dir = git_dir_with(tmp.path(), &["index.lock"]);
        assert!(!markers_present(&git_dir));
    }

    #[test]
    fn classify_recognises_index_lock_marker_and_head() {
        let tmp = tempfile::tempdir().unwrap();
        let git_dir = tmp.path().join(".git");
        let common_dir = git_dir.clone();

        assert_eq!(
            classify_git_dir_path(&git_dir, &common_dir, &git_dir.join("index.lock")),
            GitDirSignal::IndexLockNoise
        );
        assert_eq!(
            classify_git_dir_path(&git_dir, &common_dir, &git_dir.join("rebase-merge")),
            GitDirSignal::Marker
        );
        assert_eq!(
            classify_git_dir_path(&git_dir, &common_dir, &git_dir.join("MERGE_HEAD")),
            GitDirSignal::Marker
        );
        assert_eq!(
            classify_git_dir_path(&git_dir, &common_dir, &git_dir.join("HEAD")),
            GitDirSignal::HeadCandidate
        );
        assert_eq!(
            classify_git_dir_path(&git_dir, &common_dir, &git_dir.join("logs").join("HEAD")),
            GitDirSignal::HeadCandidate
        );
        assert_eq!(
            classify_git_dir_path(&git_dir, &common_dir, &common_dir.join("packed-refs")),
            GitDirSignal::HeadCandidate
        );
        assert_eq!(
            classify_git_dir_path(&git_dir, &common_dir, &git_dir.join("index")),
            GitDirSignal::Ignored
        );
        assert_eq!(
            classify_git_dir_path(&git_dir, &common_dir, &git_dir.join("COMMIT_EDITMSG")),
            GitDirSignal::Ignored
        );
    }

    /// The load-bearing case (matrix 5): a marker under repo A's `git_dir`
    /// must NOT classify as a marker/HEAD-candidate against repo B's
    /// `git_dir`/`common_dir`, even when B's `common_dir` happens to equal
    /// A's `git_dir` (the linked-worktree shared-common-dir shape).
    #[test]
    fn classify_is_repo_specific_not_basename_only() {
        let main_dir = PathBuf::from("/repo/.git");
        let linked_git_dir = main_dir.join("worktrees").join("wt");
        let linked_common_dir = main_dir.clone();

        // main's rebase-merge marker.
        let path = main_dir.join("rebase-merge");
        assert_eq!(
            classify_git_dir_path(&main_dir, &main_dir, &path),
            GitDirSignal::Marker,
            "main must see its own marker"
        );
        assert_eq!(
            classify_git_dir_path(&linked_git_dir, &linked_common_dir, &path),
            GitDirSignal::Ignored,
            "linked must NOT see main's marker as its own"
        );

        // main's HEAD file.
        let head = main_dir.join("HEAD");
        assert_eq!(
            classify_git_dir_path(&main_dir, &main_dir, &head),
            GitDirSignal::HeadCandidate
        );
        assert_eq!(
            classify_git_dir_path(&linked_git_dir, &linked_common_dir, &head),
            GitDirSignal::Ignored,
            "main's own HEAD file is not linked's HEAD (linked's HEAD lives at \
             .git/worktrees/wt/HEAD, a different path)"
        );

        // The genuinely shared packed-refs IS a candidate for both.
        let packed = main_dir.join("packed-refs");
        assert_eq!(
            classify_git_dir_path(&main_dir, &main_dir, &packed),
            GitDirSignal::HeadCandidate
        );
        assert_eq!(
            classify_git_dir_path(&linked_git_dir, &linked_common_dir, &packed),
            GitDirSignal::HeadCandidate,
            "packed-refs is genuinely shared — both repos treat it as a signal \
             to re-check their OWN head, which is a no-op if unchanged"
        );
    }

    #[test]
    fn new_gate_starts_idle_with_no_history() {
        let gate = RepoGate::new();
        assert!(!gate.suspended);
        assert!(gate.last_head.is_none());
        assert!(gate.held.is_empty());
    }

    // --- detect_op (Phase G-server's `GET /api/repo-state` reuse) --------

    #[test]
    fn detect_op_reports_none_for_a_plain_git_dir() {
        let tmp = tempfile::tempdir().unwrap();
        let git_dir = tmp.path().join(".git");
        std::fs::create_dir_all(&git_dir).unwrap();
        let (op, detail) = detect_op(&git_dir);
        assert_eq!(op, RepoOp::None);
        assert_eq!(detail, OpDetail::default());
    }

    #[test]
    fn detect_op_reports_rebase_merge_step_and_total() {
        let tmp = tempfile::tempdir().unwrap();
        let git_dir = tmp.path().join(".git");
        let rebase_merge = git_dir.join("rebase-merge");
        std::fs::create_dir_all(&rebase_merge).unwrap();
        std::fs::write(rebase_merge.join("msgnum"), "2\n").unwrap();
        std::fs::write(rebase_merge.join("end"), "5\n").unwrap();
        let (op, detail) = detect_op(&git_dir);
        assert_eq!(op, RepoOp::Rebase);
        assert_eq!(detail.step, Some(2));
        assert_eq!(detail.total, Some(5));
        assert_eq!(detail.head_sha, None);
    }

    #[test]
    fn detect_op_reports_rebase_apply_step_and_total() {
        let tmp = tempfile::tempdir().unwrap();
        let git_dir = tmp.path().join(".git");
        let rebase_apply = git_dir.join("rebase-apply");
        std::fs::create_dir_all(&rebase_apply).unwrap();
        std::fs::write(rebase_apply.join("next"), "1\n").unwrap();
        std::fs::write(rebase_apply.join("last"), "3\n").unwrap();
        let (op, detail) = detect_op(&git_dir);
        assert_eq!(op, RepoOp::Rebase);
        assert_eq!(detail.step, Some(1));
        assert_eq!(detail.total, Some(3));
    }

    #[test]
    fn detect_op_reports_merge_head_sha() {
        let tmp = tempfile::tempdir().unwrap();
        let git_dir = tmp.path().join(".git");
        std::fs::create_dir_all(&git_dir).unwrap();
        std::fs::write(git_dir.join("MERGE_HEAD"), "deadbeef\n").unwrap();
        let (op, detail) = detect_op(&git_dir);
        assert_eq!(op, RepoOp::Merge);
        assert_eq!(detail.head_sha.as_deref(), Some("deadbeef"));
        assert_eq!(detail.step, None);
    }

    #[test]
    fn detect_op_reports_cherry_pick_head_sha() {
        let tmp = tempfile::tempdir().unwrap();
        let git_dir = tmp.path().join(".git");
        std::fs::create_dir_all(&git_dir).unwrap();
        std::fs::write(git_dir.join("CHERRY_PICK_HEAD"), "cafef00d\n").unwrap();
        let (op, detail) = detect_op(&git_dir);
        assert_eq!(op, RepoOp::CherryPick);
        assert_eq!(detail.head_sha.as_deref(), Some("cafef00d"));
    }

    #[test]
    fn detect_op_reports_bisect_with_no_detail() {
        let tmp = tempfile::tempdir().unwrap();
        let git_dir = tmp.path().join(".git");
        std::fs::create_dir_all(&git_dir).unwrap();
        std::fs::write(git_dir.join("BISECT_LOG"), "git bisect start\n").unwrap();
        let (op, detail) = detect_op(&git_dir);
        assert_eq!(op, RepoOp::Bisect);
        assert_eq!(detail, OpDetail::default());
    }

    #[test]
    fn detect_op_prioritises_rebase_merge_over_a_concurrently_present_merge_head() {
        // Should never occur in a real repo (git refuses to start a merge
        // mid-rebase) — this pins the DETERMINISTIC tie-break for a
        // hand-corrupted `.git` dir rather than leaving it to HashMap-like
        // iteration order.
        let tmp = tempfile::tempdir().unwrap();
        let git_dir = tmp.path().join(".git");
        let rebase_merge = git_dir.join("rebase-merge");
        std::fs::create_dir_all(&rebase_merge).unwrap();
        std::fs::write(git_dir.join("MERGE_HEAD"), "deadbeef\n").unwrap();
        let (op, _detail) = detect_op(&git_dir);
        assert_eq!(op, RepoOp::Rebase);
    }
}
