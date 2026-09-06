//! Per-request classing — the ONE function that turns a stored lane claim
//! into a trust class (V72-H4a).
//!
//! `lane_facts` stores no class. [`class_for`] computes one on every read
//! from `min(lane ceiling, per-fact cap, anchor state)`, so:
//!
//! * a lane can never mint above its registry ceiling, whatever the anchor
//!   says (the rails-lens structural cap, generalised);
//! * a fact whose blob has moved since the tool ran can never come back
//!   `exact`, however confident the tool was;
//! * a fact that re-anchors nowhere is an **orphan** — surfaced with its
//!   original line, never dropped and never guessed onto a line.
//!
//! There is exactly one re-anchoring ladder in this crate and this module
//! calls it rather than growing a second: `annotations::anchor_for_line` +
//! `annotations::resolve` (kb-core's `fuzzy_resolve_anchor_with` under the
//! hood) with `review_comments::line_matches_snippet` as the snippet
//! guard — the same three calls review comments, findings and the GitHub
//! thread mapper make.
//!
//! ## The rules, as a table
//!
//! | condition | class | `reason` |
//! |---|---|---|
//! | the path is not in the mirror | `orphan` | `path-gone` |
//! | blob == current, `sha_source = tool` | `exact` | `blob-current` |
//! | blob == current, `sha_source = mirror_at_ingest` | `likely` | `blob-current-sha-attributed` |
//! | blob moved, file-level fact (no range) | `likely` | `file-level-blob-moved` |
//! | blob moved, content unreadable | `orphan` | `content-unreadable` |
//! | blob moved, no stored snippet | `orphan` | `no-snippet` |
//! | blob moved, snippet found verbatim | `likely` | `reanchored-exact` |
//! | blob moved, only a fuzzy match | `candidate` | `reanchored-fuzzy` |
//! | blob moved, nothing matched | `orphan` | `no-anchor` |
//!
//! Every class is then capped: `min(ceiling, per-fact cap, the row above)`.
//! The `blob-current-sha-attributed` rung is why `sha_source` exists at
//! all — the daemon attributing the mirror's blob at ingest time is NOT
//! the tool having named it, and reading that back as `exact` would be a
//! wrong `exact`, which is a release blocker.

use super::TrustClass;
use crate::annotations::{self, MatchConfidence};
use crate::review_comments;

/// `lane_facts.sha_source` — the producing tool named the blob.
pub const SHA_SOURCE_TOOL: &str = "tool";
/// `lane_facts.sha_source` — the daemon attributed the mirror's current
/// blob at ingest because the producer named none.
pub const SHA_SOURCE_MIRROR: &str = "mirror_at_ingest";

pub const REASON_PATH_GONE: &str = "path-gone";
pub const REASON_BLOB_CURRENT: &str = "blob-current";
pub const REASON_SHA_ATTRIBUTED: &str = "blob-current-sha-attributed";
pub const REASON_FILE_LEVEL_MOVED: &str = "file-level-blob-moved";
pub const REASON_CONTENT_UNREADABLE: &str = "content-unreadable";
pub const REASON_NO_SNIPPET: &str = "no-snippet";
pub const REASON_REANCHORED_EXACT: &str = "reanchored-exact";
pub const REASON_REANCHORED_FUZZY: &str = "reanchored-fuzzy";
pub const REASON_NO_ANCHOR: &str = "no-anchor";

/// Every `reason` [`class_for`] can emit — a closed vocabulary, walked by
/// its own test so a new rung cannot be added without appearing here.
pub const REASONS: &[&str] = &[
    REASON_PATH_GONE,
    REASON_BLOB_CURRENT,
    REASON_SHA_ATTRIBUTED,
    REASON_FILE_LEVEL_MOVED,
    REASON_CONTENT_UNREADABLE,
    REASON_NO_SNIPPET,
    REASON_REANCHORED_EXACT,
    REASON_REANCHORED_FUZZY,
    REASON_NO_ANCHOR,
];

/// The anchoring inputs of one stored (or freshly derived) fact.
#[derive(Debug, Clone, Copy)]
pub struct FactAnchor<'a> {
    pub blob_sha: &'a str,
    pub sha_source: &'a str,
    pub range_start: Option<u32>,
    pub range_end: Option<u32>,
    pub snippet: Option<&'a str>,
    /// A ceiling this PARTICULAR fact declares for itself, below its
    /// lane's. `git.behavior`'s `last_touch` uses it: the agent-vs-human
    /// call is `exact` when the commit carries a `Kb-Session:` trailer and
    /// `likely` when it carries none (absence of evidence is not evidence
    /// of a human), and that is a property of the fact, not the lane.
    pub cap: Option<TrustClass>,
}

/// The classed result: the class, why, and where the fact reads NOW.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Classed {
    pub class: TrustClass,
    pub reason: &'static str,
    /// The re-anchored line, or the original when nothing moved. `None`
    /// for a file-level fact. An orphan keeps its ORIGINAL line here so a
    /// caller has something honest to show beside the orphan caption.
    pub line: Option<u32>,
    pub line_end: Option<u32>,
    /// `true` when the fact re-anchored to a different line than it was
    /// produced at.
    pub shifted: bool,
}

/// The one classing function — see the module doc's table.
///
/// `current_blob` is the mirror's blob for this path RIGHT NOW (`None` =
/// the path is not in the mirror); `current_text` is that blob's bytes as
/// text (`None` = binary or unreadable). Both are supplied by the caller
/// so this stays a pure function with no store, no filesystem and no
/// clock — which is what makes the whole table testable.
pub fn class_for(
    ceiling: TrustClass,
    anchor: &FactAnchor<'_>,
    current_blob: Option<&str>,
    current_text: Option<&str>,
) -> Classed {
    let cap = |c: TrustClass| ceiling.min(anchor.cap.unwrap_or(TrustClass::Exact)).min(c);

    let Some(current_blob) = current_blob else {
        return Classed {
            class: TrustClass::Orphan,
            reason: REASON_PATH_GONE,
            line: anchor.range_start,
            line_end: anchor.range_end,
            shifted: false,
        };
    };

    if current_blob == anchor.blob_sha {
        let (class, reason) = if anchor.sha_source == SHA_SOURCE_TOOL {
            (TrustClass::Exact, REASON_BLOB_CURRENT)
        } else {
            (TrustClass::Likely, REASON_SHA_ATTRIBUTED)
        };
        return Classed {
            class: cap(class),
            reason,
            line: anchor.range_start,
            line_end: anchor.range_end,
            shifted: false,
        };
    }

    // The blob moved. A file-level fact has nothing to re-anchor: it is
    // about the FILE, which still exists, so it degrades one rung and says
    // so rather than being thrown away.
    let Some(start) = anchor.range_start else {
        return Classed {
            class: cap(TrustClass::Likely),
            reason: REASON_FILE_LEVEL_MOVED,
            line: None,
            line_end: None,
            shifted: false,
        };
    };

    let Some(text) = current_text else {
        return Classed {
            class: TrustClass::Orphan,
            reason: REASON_CONTENT_UNREADABLE,
            line: Some(start),
            line_end: anchor.range_end,
            shifted: false,
        };
    };

    let Some(snippet) = anchor.snippet.filter(|s| !s.trim().is_empty()) else {
        return Classed {
            class: TrustClass::Orphan,
            reason: REASON_NO_SNIPPET,
            line: Some(start),
            line_end: anchor.range_end,
            shifted: false,
        };
    };

    // Fast path: the anchored text is still exactly where it was. This is
    // what the ladder would return anyway (`locate_line` picks the
    // occurrence nearest the original line, which is this one), computed
    // without rendering the whole file into kb-core's synthetic HTML —
    // pinned against the ladder by `the_fast_path_agrees_with_the_ladder`.
    if review_comments::line_matches_snippet(text, start, snippet) {
        return Classed {
            class: cap(TrustClass::Likely),
            reason: REASON_REANCHORED_EXACT,
            line: Some(start),
            line_end: anchor.range_end,
            shifted: false,
        };
    }

    let a = annotations::anchor_for_line(start, snippet);
    let resolved = annotations::resolve(text, &a);
    if resolved.stale {
        return Classed {
            class: TrustClass::Orphan,
            reason: REASON_NO_ANCHOR,
            line: Some(start),
            line_end: anchor.range_end,
            shifted: false,
        };
    }
    let verbatim = matches!(resolved.confidence, MatchConfidence::Exact)
        && review_comments::line_matches_snippet(text, resolved.line, snippet);
    let (class, reason) = if verbatim {
        (TrustClass::Likely, REASON_REANCHORED_EXACT)
    } else {
        (TrustClass::Candidate, REASON_REANCHORED_FUZZY)
    };
    let delta = resolved.line as i64 - start as i64;
    let line_end = anchor
        .range_end
        .map(|e| ((e as i64 + delta).max(resolved.line as i64)) as u32);
    Classed {
        class: cap(class),
        reason,
        line: Some(resolved.line),
        line_end,
        shifted: delta != 0,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const FILE_A: &str = "one\ntwo\nthree\nfour\n";

    fn anchor<'a>(
        blob: &'a str,
        sha_source: &'a str,
        start: Option<u32>,
        snippet: Option<&'a str>,
    ) -> FactAnchor<'a> {
        FactAnchor {
            blob_sha: blob,
            sha_source,
            range_start: start,
            range_end: start,
            snippet,
            cap: None,
        }
    }

    #[test]
    fn exact_needs_both_the_current_blob_and_a_tool_named_sha() {
        let a = anchor("b1", SHA_SOURCE_TOOL, Some(2), Some("two"));
        let c = class_for(TrustClass::Exact, &a, Some("b1"), Some(FILE_A));
        assert_eq!(c.class, TrustClass::Exact);
        assert_eq!(c.reason, REASON_BLOB_CURRENT);

        let a = anchor("b1", SHA_SOURCE_MIRROR, Some(2), Some("two"));
        let c = class_for(TrustClass::Exact, &a, Some("b1"), Some(FILE_A));
        assert_eq!(
            c.class,
            TrustClass::Likely,
            "a sha the daemon attributed is not a sha the tool named"
        );
        assert_eq!(c.reason, REASON_SHA_ATTRIBUTED);
    }

    #[test]
    fn the_lane_ceiling_and_the_per_fact_cap_both_clamp() {
        let a = anchor("b1", SHA_SOURCE_TOOL, Some(2), Some("two"));
        assert_eq!(
            class_for(TrustClass::Likely, &a, Some("b1"), Some(FILE_A)).class,
            TrustClass::Likely
        );
        let capped = FactAnchor {
            cap: Some(TrustClass::Candidate),
            ..a
        };
        assert_eq!(
            class_for(TrustClass::Exact, &capped, Some("b1"), Some(FILE_A)).class,
            TrustClass::Candidate
        );
    }

    #[test]
    fn an_unrelated_edit_carries_the_fact_forward_at_likely() {
        // The blob changed; the anchored line moved down by one.
        let edited = "zero\none\ntwo\nthree\nfour\n";
        let a = anchor("b1", SHA_SOURCE_TOOL, Some(2), Some("two"));
        let c = class_for(TrustClass::Exact, &a, Some("b2"), Some(edited));
        assert_eq!(c.class, TrustClass::Likely);
        assert_eq!(c.reason, REASON_REANCHORED_EXACT);
        assert_eq!(c.line, Some(3));
        assert!(c.shifted);
    }

    #[test]
    fn an_edit_that_leaves_the_line_alone_takes_the_fast_path() {
        let edited = "one\ntwo\nTHREE CHANGED\nfour\n";
        let a = anchor("b1", SHA_SOURCE_TOOL, Some(2), Some("two"));
        let c = class_for(TrustClass::Exact, &a, Some("b2"), Some(edited));
        assert_eq!(c.class, TrustClass::Likely);
        assert_eq!(c.reason, REASON_REANCHORED_EXACT);
        assert_eq!(c.line, Some(2));
        assert!(!c.shifted);
    }

    #[test]
    fn the_fast_path_agrees_with_the_ladder() {
        // Same input, ladder run explicitly: the fast path must not be a
        // second, subtly different resolution.
        let edited = "one\ntwo\nTHREE CHANGED\nfour\n";
        let a = annotations::anchor_for_line(2, "two");
        let r = annotations::resolve(edited, &a);
        assert!(!r.stale);
        assert_eq!(r.line, 2);
        assert!(matches!(r.confidence, MatchConfidence::Exact));
    }

    #[test]
    fn a_deleted_line_is_an_honest_orphan_that_keeps_its_original_line() {
        let edited = "one\nthree\nfour\n";
        let a = anchor(
            "b1",
            SHA_SOURCE_TOOL,
            Some(2),
            Some("qqqqzzzz-nothing-like-this"),
        );
        let c = class_for(TrustClass::Exact, &a, Some("b2"), Some(edited));
        assert_eq!(c.class, TrustClass::Orphan);
        assert_eq!(c.reason, REASON_NO_ANCHOR);
        assert_eq!(c.line, Some(2));
    }

    #[test]
    fn a_deleted_path_is_an_orphan_before_anything_else_is_considered() {
        let a = anchor("b1", SHA_SOURCE_TOOL, Some(2), Some("two"));
        let c = class_for(TrustClass::Exact, &a, None, None);
        assert_eq!(c.class, TrustClass::Orphan);
        assert_eq!(c.reason, REASON_PATH_GONE);
    }

    #[test]
    fn a_moved_blob_with_no_snippet_cannot_be_re_anchored_at_all() {
        let a = anchor("b1", SHA_SOURCE_TOOL, Some(2), None);
        let c = class_for(TrustClass::Exact, &a, Some("b2"), Some(FILE_A));
        assert_eq!(c.class, TrustClass::Orphan);
        assert_eq!(c.reason, REASON_NO_SNIPPET);
    }

    #[test]
    fn a_moved_blob_with_unreadable_content_is_an_orphan_not_a_guess() {
        let a = anchor("b1", SHA_SOURCE_TOOL, Some(2), Some("two"));
        let c = class_for(TrustClass::Exact, &a, Some("b2"), None);
        assert_eq!(c.class, TrustClass::Orphan);
        assert_eq!(c.reason, REASON_CONTENT_UNREADABLE);
    }

    #[test]
    fn a_file_level_fact_degrades_one_rung_when_the_blob_moves() {
        let a = anchor("b1", SHA_SOURCE_TOOL, None, None);
        let c = class_for(TrustClass::Exact, &a, Some("b2"), Some(FILE_A));
        assert_eq!(c.class, TrustClass::Likely);
        assert_eq!(c.reason, REASON_FILE_LEVEL_MOVED);
        assert_eq!(c.line, None);
    }

    #[test]
    fn a_near_miss_re_anchors_fuzzily_at_candidate() {
        let edited = "one\ntwo but edited a little\nthree\nfour\n";
        let a = anchor("b1", SHA_SOURCE_TOOL, Some(2), Some("two but edited"));
        let c = class_for(TrustClass::Exact, &a, Some("b2"), Some(edited));
        assert!(
            matches!(c.class, TrustClass::Candidate | TrustClass::Orphan),
            "a fuzzy or absent match must never reach likely: {c:?}"
        );
        if c.class == TrustClass::Candidate {
            assert_eq!(c.reason, REASON_REANCHORED_FUZZY);
        }
    }

    #[test]
    fn every_emitted_reason_is_in_the_declared_vocabulary() {
        let cases: Vec<Classed> = vec![
            class_for(
                TrustClass::Exact,
                &anchor("b1", SHA_SOURCE_TOOL, Some(2), Some("two")),
                None,
                None,
            ),
            class_for(
                TrustClass::Exact,
                &anchor("b1", SHA_SOURCE_TOOL, Some(2), Some("two")),
                Some("b1"),
                Some(FILE_A),
            ),
            class_for(
                TrustClass::Exact,
                &anchor("b1", SHA_SOURCE_MIRROR, Some(2), Some("two")),
                Some("b1"),
                Some(FILE_A),
            ),
            class_for(
                TrustClass::Exact,
                &anchor("b1", SHA_SOURCE_TOOL, None, None),
                Some("b2"),
                Some(FILE_A),
            ),
            class_for(
                TrustClass::Exact,
                &anchor("b1", SHA_SOURCE_TOOL, Some(2), Some("two")),
                Some("b2"),
                None,
            ),
            class_for(
                TrustClass::Exact,
                &anchor("b1", SHA_SOURCE_TOOL, Some(2), None),
                Some("b2"),
                Some(FILE_A),
            ),
            class_for(
                TrustClass::Exact,
                &anchor("b1", SHA_SOURCE_TOOL, Some(2), Some("two")),
                Some("b2"),
                Some("zero\none\ntwo\nthree\n"),
            ),
            class_for(
                TrustClass::Exact,
                &anchor("b1", SHA_SOURCE_TOOL, Some(2), Some("qqqqzzzz-nothing")),
                Some("b2"),
                Some("one\nthree\n"),
            ),
        ];
        for c in cases {
            assert!(
                REASONS.contains(&c.reason),
                "{:?} is not in REASONS",
                c.reason
            );
        }
    }
}
