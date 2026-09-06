//! `comments/1` — the drift oracle (D8's "per-request `doc_state` from
//! blame").
//!
//! Pure arithmetic over `git blame`, computed per request and PERSISTED
//! NOWHERE. Three rules, in the order they matter:
//!
//! 1. **It is never a verdict.** `drifted` says "the newest commit under
//!    the documented body is newer than the newest commit under the doc
//!    block, by N days", and names both commits so the caller can check.
//!    It does not say the comment is wrong. A comment that describes
//!    INTENT legitimately survives a refactor; deciding whether this one
//!    still tells the truth is an agent-layer step (root CLAUDE.md's
//!    no-in-daemon-LLM non-goal), not a number this daemon can compute.
//! 2. **It refuses rather than guesses.** No blame for a range, an
//!    uncommitted line anywhere in either range, a file the per-request
//!    blame budget did not reach — every one of those is
//!    `unknown` WITH ITS REASON, never a silent `fresh`.
//! 3. **Each kind gets only the state its own evidence supports.** A
//!    `doc` block gets `fresh`/`drifted`/`unknown`; an `annotation`
//!    carrying a smart_todo `on: date(...)` gets `aged` once that date is
//!    past; a SUPPRESSION directive with no reason gets `unreasoned`.
//!    Everything else gets `none` — the honest "this block has no state",
//!    not a flattering `fresh`.

use crate::blame::BlameRegion;
use crate::comments::keywords::SmartTodoFields;
use crate::provenance::UNCOMMITTED_SHA;
use serde::Serialize;

/// The closed state vocabulary — the `?state=` filter's values and the
/// summary's count keys.
pub const STATE_NAMES: &[&str] = &["none", "fresh", "drifted", "unknown", "aged", "unreasoned"];

/// The subset an operator (or an agent) can act on — `kb-code comments
/// audit`'s slice and the dashboard's narrow default.
pub const ACTIONABLE_STATES: &[&str] = &["drifted", "aged", "unreasoned"];

/// One block's computed state. A struct rather than an enum on the wire:
/// every consumer reads `state` first and the decomposition second, and a
/// field that does not apply is ABSENT rather than null-and-meaningless.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Default)]
pub struct CommentState {
    /// One of [`STATE_NAMES`].
    pub state: &'static str,
    /// Only for `unknown` — why the oracle refused.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<&'static str>,
    /// `drifted`: whole days by which the code's newest commit is newer
    /// than the doc's. `aged`: whole days the `on:` date is past.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub age_days: Option<i64>,
    /// `drifted` — the newest commit touching the documented body.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub code_commit: Option<String>,
    /// `drifted` — the newest commit touching the doc block itself.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub doc_commit: Option<String>,
    /// `aged` — the smart_todo date that has passed.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub on_date: Option<String>,
    /// `unreasoned` — the tool whose suppression carries no justification.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool: Option<String>,
}

impl CommentState {
    pub fn none() -> Self {
        Self {
            state: "none",
            ..Default::default()
        }
    }

    pub fn fresh() -> Self {
        Self {
            state: "fresh",
            ..Default::default()
        }
    }

    pub fn unknown(reason: &'static str) -> Self {
        Self {
            state: "unknown",
            reason: Some(reason),
            ..Default::default()
        }
    }
}

/// One file's blame, indexed for range queries. Built once per file per
/// request; dropped when the response is written.
#[derive(Debug, Clone, Default)]
pub struct FileBlame {
    regions: Vec<BlameRegion>,
}

impl FileBlame {
    pub fn new(regions: Vec<BlameRegion>) -> Self {
        Self { regions }
    }

    /// The newest `(author_time, sha)` over the 1-based inclusive line
    /// range, plus whether ANY covered line is uncommitted.
    ///
    /// `None` when no region covers the range at all — a file blamed at a
    /// revision that predates these lines, or a range past EOF. That is a
    /// refusal, not a zero.
    pub fn newest_in(&self, start: u32, end: u32) -> Option<(i64, String, bool)> {
        let mut best: Option<(i64, String)> = None;
        let mut uncommitted = false;
        for r in &self.regions {
            let r_start = r.final_start;
            let r_end = r.final_start.saturating_add(r.count.saturating_sub(1));
            if r_end < start || r_start > end {
                continue;
            }
            if r.sha == UNCOMMITTED_SHA {
                uncommitted = true;
            }
            match &best {
                Some((t, _)) if *t >= r.author_time => {}
                _ => best = Some((r.author_time, r.sha.clone())),
            }
        }
        best.map(|(t, sha)| (t, sha, uncommitted))
    }
}

/// The `doc` state for one block: compare the newest commit under the doc
/// lines against the newest commit under the documented body.
pub fn doc_state(
    blame: &FileBlame,
    doc_start: u32,
    doc_end: u32,
    body_start: u32,
    body_end: u32,
) -> CommentState {
    let Some((doc_time, doc_sha, doc_dirty)) = blame.newest_in(doc_start, doc_end) else {
        return CommentState::unknown("no-blame-for-doc-lines");
    };
    let Some((code_time, code_sha, code_dirty)) = blame.newest_in(body_start, body_end) else {
        return CommentState::unknown("no-blame-for-symbol-body");
    };
    if doc_dirty || code_dirty {
        return CommentState::unknown("uncommitted");
    }
    if code_time > doc_time {
        return CommentState {
            state: "drifted",
            age_days: Some((code_time - doc_time) / 86_400),
            code_commit: Some(code_sha),
            doc_commit: Some(doc_sha),
            ..Default::default()
        };
    }
    CommentState::fresh()
}

/// The `annotation` state: `aged` once a smart_todo `on: date('…')` is
/// strictly in the past. Every other predicate shape (`issue_close`,
/// `pull_request_close`, `gem_bump`, `gem_release`) resolves only over the
/// network, which this daemon does not do — so those are `none`, never a
/// guess.
pub fn annotation_state(
    fields: Option<&SmartTodoFields>,
    today: chrono::NaiveDate,
) -> CommentState {
    let Some(f) = fields else {
        return CommentState::none();
    };
    let Some(on_date) = f.on_date.as_deref() else {
        return CommentState::none();
    };
    let Ok(due) = chrono::NaiveDate::parse_from_str(on_date, "%Y-%m-%d") else {
        return CommentState::unknown("unparseable-on-date");
    };
    if due < today {
        return CommentState {
            state: "aged",
            age_days: Some((today - due).num_days()),
            on_date: Some(on_date.to_string()),
            ..Default::default()
        };
    }
    CommentState::none()
}

/// The `directive` state: `unreasoned` for a SUPPRESSION directive that
/// carries no justification. `has_reason` is `None` for a magic comment /
/// build tag, which has nothing to justify — those are `none`.
pub fn directive_state(tool: Option<&str>, has_reason: Option<bool>) -> CommentState {
    match has_reason {
        Some(false) => CommentState {
            state: "unreasoned",
            tool: tool.map(str::to_string),
            ..Default::default()
        },
        _ => CommentState::none(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn region(sha: &str, final_start: u32, count: u32, t: i64) -> BlameRegion {
        BlameRegion {
            sha: sha.to_string(),
            orig_start: final_start,
            final_start,
            count,
            author_time: t,
            ..Default::default()
        }
    }

    const DAY: i64 = 86_400;

    #[test]
    fn state_vocabulary_is_closed_and_the_actionable_slice_is_a_subset() {
        for s in ACTIONABLE_STATES {
            assert!(STATE_NAMES.contains(s), "{s} is not a declared state");
        }
    }

    #[test]
    fn a_doc_newer_than_its_body_is_fresh() {
        let b = FileBlame::new(vec![
            region("aaa", 1, 2, 200 * DAY),
            region("bbb", 3, 5, 100 * DAY),
        ]);
        assert_eq!(doc_state(&b, 1, 2, 3, 7).state, "fresh");
    }

    #[test]
    fn a_body_newer_than_its_doc_is_drifted_with_the_arithmetic_shown() {
        let b = FileBlame::new(vec![
            region("aaa", 1, 2, 100 * DAY),
            region("bbb", 3, 5, 114 * DAY),
        ]);
        let s = doc_state(&b, 1, 2, 3, 7);
        assert_eq!(s.state, "drifted");
        assert_eq!(s.age_days, Some(14));
        assert_eq!(s.code_commit.as_deref(), Some("bbb"));
        assert_eq!(s.doc_commit.as_deref(), Some("aaa"));
    }

    #[test]
    fn equal_times_are_fresh_not_drifted() {
        // The doc and the code landed in the same commit — the ONE case
        // where "unchanged since" is provable.
        let b = FileBlame::new(vec![region("aaa", 1, 9, 100 * DAY)]);
        assert_eq!(doc_state(&b, 1, 2, 3, 7).state, "fresh");
    }

    #[test]
    fn an_uncommitted_line_in_either_range_is_unknown_never_fresh() {
        let b = FileBlame::new(vec![
            region(UNCOMMITTED_SHA, 1, 2, 300 * DAY),
            region("bbb", 3, 5, 100 * DAY),
        ]);
        let s = doc_state(&b, 1, 2, 3, 7);
        assert_eq!(s.state, "unknown");
        assert_eq!(s.reason, Some("uncommitted"));
    }

    #[test]
    fn a_range_no_region_covers_is_unknown_with_the_side_that_failed() {
        let b = FileBlame::new(vec![region("aaa", 1, 2, 100 * DAY)]);
        assert_eq!(
            doc_state(&b, 1, 2, 40, 50).reason,
            Some("no-blame-for-symbol-body")
        );
        assert_eq!(
            doc_state(&b, 40, 50, 1, 2).reason,
            Some("no-blame-for-doc-lines")
        );
    }

    #[test]
    fn an_empty_blame_is_unknown_not_an_empty_verdict() {
        let b = FileBlame::default();
        assert_eq!(doc_state(&b, 1, 2, 3, 4).state, "unknown");
    }

    #[test]
    fn a_past_smart_todo_date_is_aged_and_a_future_one_is_none() {
        let today = chrono::NaiveDate::from_ymd_opt(2026, 9, 6).unwrap();
        let past = SmartTodoFields {
            raw: Default::default(),
            on_kind: Some("date".into()),
            on_date: Some("2026-08-27".into()),
            to: None,
        };
        let s = annotation_state(Some(&past), today);
        assert_eq!(s.state, "aged");
        assert_eq!(s.age_days, Some(10));
        assert_eq!(s.on_date.as_deref(), Some("2026-08-27"));

        let future = SmartTodoFields {
            on_date: Some("2027-01-01".into()),
            ..past.clone()
        };
        assert_eq!(annotation_state(Some(&future), today).state, "none");
        // Today itself is not yet past.
        let today_due = SmartTodoFields {
            on_date: Some("2026-09-06".into()),
            ..past.clone()
        };
        assert_eq!(annotation_state(Some(&today_due), today).state, "none");
    }

    #[test]
    fn a_non_date_predicate_is_none_because_this_daemon_makes_no_network_call() {
        let today = chrono::NaiveDate::from_ymd_opt(2026, 9, 6).unwrap();
        let f = SmartTodoFields {
            raw: Default::default(),
            on_kind: Some("issue_close".into()),
            on_date: None,
            to: None,
        };
        assert_eq!(annotation_state(Some(&f), today).state, "none");
        assert_eq!(annotation_state(None, today).state, "none");
    }

    #[test]
    fn only_a_reasonless_suppression_is_unreasoned() {
        assert_eq!(
            directive_state(Some("rubocop"), Some(false)).state,
            "unreasoned"
        );
        assert_eq!(directive_state(Some("rubocop"), Some(true)).state, "none");
        // A magic comment has nothing to justify.
        assert_eq!(directive_state(Some("ruby-magic"), None).state, "none");
    }
}
