//! `GET /api/branches` — ahead/behind + tip attribution (Phase C3's server
//! half) and V4.L1's suggested ranking. This module owns the ahead/behind
//! arithmetic ([`ahead_behind`]) and the deterministic [`suggest`] score
//! (terms are the substance). `routes::branches_route` assembles the rest
//! (local+remote listing via `GitRepo::list_refs`/`list_remote_branches`,
//! default via `git::default_branch`, tip attribution via `join::ladder::
//! resolve_commit` — ASYNC, so it can't run inside this module's blocking
//! git calls; see that route's own doc for how the two halves are stitched
//! together).
//!
//! Ahead/behind costs exactly ONE `git rev-list --left-right --count
//! <default>...<branch>` subprocess call per non-default branch — the
//! route handler caps the branch list at [`MAX_BRANCHES`] for `name`-sort
//! (truncates before this call) or `3×MAX_BRANCHES` for `suggested`-sort
//! (V70-A3X two-phase widening: ranks on cheap gix/store terms first,
//! widens to 3× before running rev-list so the FULL score has a fair shot
//! at surfacing a cheap-mediocre-but-expensive-strong branch, then
//! re-ranks and truncates to `MAX_BRANCHES` for the response — see
//! `routes::branches_route`'s own doc) and skips this call entirely for
//! the default branch itself (defined as 0 ahead / 0 behind of itself).

use super::{run_git_raw, HistoryError, Result};
use crate::git::RefRange;
#[cfg(test)]
use crate::git::Revspec;
use std::collections::BTreeMap;
use std::path::Path;

/// Branch-list cap — `routes::branches_route` truncates to this many
/// (name-sort: by name before the cap; suggested: cheap-rank then cap)
/// and reports `truncated: true` when more existed, so a runaway
/// ahead/behind subprocess fan-out is bounded regardless of how many
/// branches a repo actually has.
pub const MAX_BRANCHES: usize = 100;

/// Recency half-life for [`suggest`]'s `recency` term (14 days).
pub const RECENCY_HALF_LIFE_SECS: f32 = 14.0 * 24.0 * 3600.0;

/// Modest weight on `ln(1 + ahead)` so a long-lived unique-commit count
/// cannot drown recency / an open review.
const AHEAD_WEIGHT: f32 = 0.15;
/// Smaller penalty on `ln(1 + behind)` — a stale branch ranks a bit
/// lower, but behind never dominates the other terms.
const BEHIND_WEIGHT: f32 = 0.05;

/// `sort=suggested` payload — `score` is the sum of `terms`. Unmeasured
/// signals stay out of the map (never a silent 0).
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct Suggest {
    pub score: f32,
    pub terms: BTreeMap<String, f32>,
}

/// Inputs to [`suggest`]. `ahead`/`behind` are `None` for the cheap
/// pre-cap ranking (rev-list has not run yet); the route fills them in
/// for survivors so the returned terms name every computed signal.
#[derive(Debug, Clone)]
pub struct SuggestInput {
    pub now_unix: i64,
    pub author_time: Option<i64>,
    pub has_open_review: bool,
    /// Join-ladder confidence mapped to a weight (`None` = unmeasured /
    /// `"none"`). The mapping lives at the route so this module stays
    /// free of the ladder types.
    pub attribution: Option<f32>,
    pub ahead: Option<u32>,
    pub behind: Option<u32>,
}

/// Deterministic suggested-branch score. Terms name the contributing
/// signals (house convention: never a bare score).
pub fn suggest(input: &SuggestInput) -> Suggest {
    let mut terms = BTreeMap::new();
    if let Some(author_time) = input.author_time {
        let age = input.now_unix.saturating_sub(author_time).max(0) as f32;
        let recency = 2f32.powf(-age / RECENCY_HALF_LIFE_SECS);
        terms.insert("recency".to_string(), recency);
    }
    if input.has_open_review {
        terms.insert("has_open_review".to_string(), 1.0);
    }
    if let Some(weight) = input.attribution {
        if weight > 0.0 {
            terms.insert("attribution".to_string(), weight);
        }
    }
    if let Some(ahead) = input.ahead {
        terms.insert(
            "ahead".to_string(),
            (1.0 + ahead as f32).ln() * AHEAD_WEIGHT,
        );
    }
    if let Some(behind) = input.behind {
        terms.insert(
            "behind".to_string(),
            -(1.0 + behind as f32).ln() * BEHIND_WEIGHT,
        );
    }
    let score = terms.values().copied().sum();
    Suggest { score, terms }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
pub struct AheadBehind {
    pub ahead: u32,
    pub behind: u32,
}

/// `git rev-list --left-right --count <default>...<branch>` — output is
/// `<left>\t<right>`: LEFT counts commits reachable from `default` but not
/// `branch` (how far `branch` is BEHIND `default`); RIGHT counts the
/// reverse (how far `branch` is AHEAD of `default`).
pub fn ahead_behind(repo_root: &Path, range: &RefRange) -> Result<AheadBehind> {
    // V70-A2 (SEC-17) — the `{default}...{branch}` string this fn used to
    // interpolate by hand is now a `RefRange` the CALLER validated
    // endpoint-wise; `as_arg` reassembles it from the validated parts, so
    // neither side can smuggle a `..`/`@{`/flag through the join.
    let revspec = range.as_arg();
    let out = run_git_raw(
        repo_root,
        &["rev-list", "--left-right", "--count", &revspec],
    )?;
    let text = String::from_utf8_lossy(&out).trim().to_string();
    let mut parts = text.splitn(2, '\t');
    let behind: u32 = parts
        .next()
        .and_then(|s| s.parse().ok())
        .ok_or_else(|| malformed_left_right_count(&text))?;
    let ahead: u32 = parts
        .next()
        .and_then(|s| s.parse().ok())
        .ok_or_else(|| malformed_left_right_count(&text))?;
    Ok(AheadBehind { ahead, behind })
}

fn malformed_left_right_count(text: &str) -> HistoryError {
    HistoryError::GitFailed {
        status: -1,
        stderr: format!("malformed `git rev-list --left-right --count` output: {text:?}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::Command as StdCommand;

    /// `<a>...<b>` from two fixture branch names — the shape
    /// `ahead_behind` now takes (V70-A2).
    fn three_dot(a: &str, b: &str) -> RefRange {
        RefRange::new(
            Revspec::parse(a).expect("fixture ref parses"),
            Revspec::parse(b).expect("fixture ref parses"),
            true,
        )
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

    #[test]
    fn ahead_behind_counts_commits_unique_to_each_side() {
        let tmp = init_repo();
        let dir = tmp.path();
        std::fs::write(dir.join("a.txt"), "base\n").unwrap();
        git(dir, &["add", "-A"]);
        git(dir, &["commit", "-q", "-m", "base"]);
        git(dir, &["branch", "feature"]);
        git(dir, &["checkout", "-q", "feature"]);
        std::fs::write(dir.join("b.txt"), "f1\n").unwrap();
        git(dir, &["add", "-A"]);
        git(dir, &["commit", "-q", "-m", "f1"]);
        std::fs::write(dir.join("c.txt"), "f2\n").unwrap();
        git(dir, &["add", "-A"]);
        git(dir, &["commit", "-q", "-m", "f2"]);
        git(dir, &["checkout", "-q", "main"]);
        std::fs::write(dir.join("d.txt"), "m1\n").unwrap();
        git(dir, &["add", "-A"]);
        git(dir, &["commit", "-q", "-m", "m1"]);

        let ab = ahead_behind(dir, &three_dot("main", "feature")).unwrap();
        assert_eq!(ab.ahead, 2, "feature has 2 commits main lacks");
        assert_eq!(ab.behind, 1, "feature lacks main's 1 own commit");
    }

    #[test]
    fn ahead_behind_is_zero_for_identical_refs() {
        let tmp = init_repo();
        let dir = tmp.path();
        std::fs::write(dir.join("a.txt"), "base\n").unwrap();
        git(dir, &["add", "-A"]);
        git(dir, &["commit", "-q", "-m", "base"]);

        let ab = ahead_behind(dir, &three_dot("main", "main")).unwrap();
        assert_eq!(
            ab,
            AheadBehind {
                ahead: 0,
                behind: 0
            }
        );
    }

    #[test]
    fn suggest_recency_is_monotone_in_author_time() {
        let now = 1_700_000_000;
        let older = suggest(&SuggestInput {
            now_unix: now,
            author_time: Some(now - 90 * 24 * 3600),
            has_open_review: false,
            attribution: None,
            ahead: None,
            behind: None,
        });
        let newer = suggest(&SuggestInput {
            now_unix: now,
            author_time: Some(now - 3600),
            has_open_review: false,
            attribution: None,
            ahead: None,
            behind: None,
        });
        let older_r = older.terms["recency"];
        let newer_r = newer.terms["recency"];
        assert!(
            newer_r > older_r,
            "newer tip must outrank an older one: {newer_r} vs {older_r}"
        );
        assert!(newer.score > older.score);
        assert!(older.terms.contains_key("recency"));
        assert!(newer.terms.contains_key("recency"));
    }

    #[test]
    fn suggest_open_review_boosts_the_score() {
        let now = 1_700_000_000;
        let base = SuggestInput {
            now_unix: now,
            author_time: Some(now),
            has_open_review: false,
            attribution: None,
            ahead: None,
            behind: None,
        };
        let closed = suggest(&base);
        let mut open_in = base;
        open_in.has_open_review = true;
        let open = suggest(&open_in);
        assert_eq!(open.terms.get("has_open_review").copied(), Some(1.0));
        assert!(!closed.terms.contains_key("has_open_review"));
        assert!(open.score > closed.score);
    }

    #[test]
    fn suggest_score_equals_the_sum_of_terms() {
        let s = suggest(&SuggestInput {
            now_unix: 1_700_000_000,
            author_time: Some(1_700_000_000),
            has_open_review: true,
            attribution: Some(0.5),
            ahead: Some(3),
            behind: Some(1),
        });
        let sum: f32 = s.terms.values().copied().sum();
        assert!(
            (s.score - sum).abs() < f32::EPSILON,
            "score {} != sum of terms {}",
            s.score,
            sum
        );
        for key in [
            "recency",
            "has_open_review",
            "attribution",
            "ahead",
            "behind",
        ] {
            assert!(s.terms.contains_key(key), "missing term {key}");
        }
    }
}
