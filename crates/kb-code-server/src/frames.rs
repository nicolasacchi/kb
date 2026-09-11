//! V75-M1 — `kbc-frames/1`: D14's per-lane **frame table**, published as
//! fact.
//!
//! Every address in kb-code v7.5 gains an `@<ref>` (absent = the working
//! tree, byte-identical to today). Off the working tree the instrument
//! degrades — and D14's rule is that it degrades *honestly*, from ONE
//! tested table rather than from a banner somebody wrote by hand on each
//! surface:
//!
//! > text/symbols/files lanes = working tree via the mirror; tree and
//! > file-at-ref = ODB; blame = git; usages likely/candidate off-HEAD;
//! > lsp-live refused
//!
//! [`FRAMES`] is that table as a Rust `const`, served verbatim at
//! `GET /api/frames` and golden-pinned
//! (`tests/fixtures/frames.golden.json`). **This unit lays the table; it
//! does not consume it** — the reader's `@ref` chip, compare mode and the
//! per-lane banners are M4's, and every one of them must DERIVE its text
//! from this response rather than restating it. That is the same
//! one-projection-two-renderers rule invariant 17(a) states for
//! `kbc-tree/1`, applied before the second renderer exists, so it cannot
//! be broken later by accident.
//!
//! Three things the table deliberately does NOT say, because saying them
//! would be a claim this daemon cannot back:
//!
//! * it is not a *capability* matrix. `syntax/1`'s Parity Grid
//!   (invariant 18(c)) answers "what can this language do"; this answers
//!   "what does this lane read, and what may it claim, when you move off
//!   the working tree". Two axes, two tables.
//! * a `refused` row is not a bug report. `lsp-live` is refused off-HEAD
//!   because kb-lip's blob guard hashes the bytes ON DISK before and after
//!   every LSP round trip — there is no honest way to ask a language
//!   server about a blob that is not checked out, and a lane that answered
//!   anyway would mint a wrong `exact`, the release blocker.
//! * a ceiling is a CEILING. `usages` at `likely` off-HEAD means the lane
//!   may not exceed `likely`; it does not promise `likely`, and a lane
//!   whose own evidence is weaker still reports what its own classer
//!   minted.

use crate::entities::RouteContract;

/// Where a lane's answer comes from when a ref is in play.
///
/// A closed vocabulary of four, on purpose: three real sources and one
/// refusal. A fifth would mean a fifth thing that can be stale.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FrameSource {
    /// The live working tree, read through the mirror index (`files` +
    /// the blob-keyed derived tables). Only ever answers for the ref that
    /// is CHECKED OUT.
    WorkingTree,
    /// The object database, read at the named ref. Answers for any ref
    /// without touching the checkout.
    Odb,
    /// A `git` subprocess that takes the ref itself.
    Git,
    /// The lane cannot answer off the working tree at all, and says so.
    Refused,
}

impl FrameSource {
    pub fn as_str(self) -> &'static str {
        match self {
            FrameSource::WorkingTree => "working_tree",
            FrameSource::Odb => "odb",
            FrameSource::Git => "git",
            FrameSource::Refused => "refused",
        }
    }
}

/// The highest trust class a lane may mint for an address that is NOT the
/// checked-out HEAD.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OffHeadClass {
    /// The lane's own classer is unaffected by the ref — it can still
    /// reach `exact` because its evidence is the ref's own bytes.
    Exact,
    /// Capped at `likely`.
    Likely,
    /// Capped at `candidate`.
    Candidate,
    /// The lane does not answer.
    Refused,
}

impl OffHeadClass {
    pub fn as_str(self) -> &'static str {
        match self {
            OffHeadClass::Exact => "exact",
            OffHeadClass::Likely => "likely",
            OffHeadClass::Candidate => "candidate",
            OffHeadClass::Refused => "refused",
        }
    }
}

/// One lane's frame contract.
#[derive(Debug, Clone, Copy, serde::Serialize)]
pub struct Frame {
    /// The lane, named as the route/verb that serves it.
    pub lane: &'static str,
    pub source: FrameSource,
    /// The lane's ceiling off HEAD. **A ceiling, never a floor.**
    pub off_head: OffHeadClass,
    /// Whether asking for a non-HEAD ref changes the answer at all. `false`
    /// means the lane reads the working tree regardless and a ref is
    /// therefore ignored — which the reader must SAY, not silently do.
    pub ref_aware: bool,
    /// Why. Every row carries one: an unexplained degrade is not a map
    /// (invariant 18(c)'s rule, applied to this table).
    pub why: &'static str,
}

/// D14's table. ORDER IS PART OF THE GOLDEN — a reader renders these rows
/// top to bottom and a silent reorder would move a banner under the
/// operator.
pub const FRAMES: &[Frame] = &[
    Frame {
        lane: "text",
        source: FrameSource::WorkingTree,
        off_head: OffHeadClass::Refused,
        ref_aware: false,
        why: "the text lane searches the mirror's indexed working-tree content; there is no \
              ref-scoped full-text index, and grepping the ODB per keystroke is not a budget \
              this daemon has",
    },
    Frame {
        lane: "symbols",
        source: FrameSource::WorkingTree,
        off_head: OffHeadClass::Refused,
        ref_aware: false,
        why: "symbol rows are keyed by (blob_hash, salt) and reachable only through the mirror's \
              path -> blob map, which points at the CHECKED-OUT blob; a ref whose blobs were \
              never indexed has no rows to return",
    },
    Frame {
        lane: "files",
        source: FrameSource::WorkingTree,
        off_head: OffHeadClass::Refused,
        ref_aware: false,
        why: "the files lane ranks the mirror's path set plus its own recency blend, both \
              properties of the checkout",
    },
    Frame {
        lane: "tree",
        source: FrameSource::Odb,
        off_head: OffHeadClass::Exact,
        ref_aware: true,
        why: "a tree listing at a ref is an ODB read and is exact by construction: the ref names \
              the tree object",
    },
    Frame {
        lane: "file_at_ref",
        source: FrameSource::Odb,
        off_head: OffHeadClass::Exact,
        ref_aware: true,
        why: "the blob at a ref is the blob; highlight spans still come from the store and are \
              null for a blob nothing indexed, which GET /api/file already reports",
    },
    Frame {
        lane: "blame",
        source: FrameSource::Git,
        off_head: OffHeadClass::Exact,
        ref_aware: true,
        why: "git blame takes the ref itself, so an off-HEAD blame is as exact as an on-HEAD one",
    },
    Frame {
        lane: "usages",
        source: FrameSource::WorkingTree,
        off_head: OffHeadClass::Likely,
        ref_aware: false,
        why: "usages resolve through occurrence rows derived from the CHECKED-OUT blobs; read \
              against another ref they describe code that may have moved, so the engine's exact \
              rung is unreachable and every row is capped at likely",
    },
    Frame {
        lane: "framework_edges",
        source: FrameSource::WorkingTree,
        off_head: OffHeadClass::Candidate,
        ref_aware: false,
        why: "rails-lens/1 edges are convention-derived and already capped at likely on HEAD \
              (invariant 20(b)); off HEAD the witness blob is not the one on disk, which demotes \
              the whole lane one rung",
    },
    Frame {
        lane: "lsp_live",
        source: FrameSource::Refused,
        off_head: OffHeadClass::Refused,
        ref_aware: true,
        why: "kb-lip hashes the bytes ON DISK before and after every LSP round trip and refuses \
              on a mismatch — that guard is what lets lsp-live mint exact at all, and it cannot \
              be satisfied for a blob that is not checked out",
    },
];

pub const FRAMES_ROUTE: RouteContract = RouteContract {
    path: "/api/frames",
    handler: "frames::frames_route",
    required_params: &[],
    params_accept_without: |_| true,
};

#[derive(Debug, serde::Serialize)]
pub struct FramesResponse {
    pub schema: &'static str,
    /// The vocabularies, so a client renders the table without hardcoding
    /// either closed set.
    pub sources: Vec<&'static str>,
    pub classes: Vec<&'static str>,
    pub frames: &'static [Frame],
    pub note: &'static str,
}

pub fn frames_response() -> FramesResponse {
    FramesResponse {
        schema: "kbc-frames/1",
        sources: vec!["working_tree", "odb", "git", "refused"],
        classes: vec!["exact", "likely", "candidate", "refused"],
        frames: FRAMES,
        note: "off_head is a CEILING, not a promise: a lane may report a class below it. \
               ref_aware=false means the lane ignores a ref and answers for the checkout — \
               a reader must say so rather than silently substituting.",
    }
}

pub async fn frames_route() -> axum::Json<FramesResponse> {
    axum::Json(frames_response())
}

/// RFC 7807 `type` for a revspec that parsed but did not resolve.
pub const ERR_UNKNOWN_REF: &str = "urn:kb:errors:unknown-ref";

/// Per-request frame claim that rides a file-at-ref (or sibling) read.
///
/// Vocabulary is the V75-M1 table's, not a second mapping: `source` is
/// `working_tree` | `odb` | `git` | `refused`. A reader generates banners
/// from these fields plus the matching [`FRAMES`] row's `why`; it must not
/// invent a parallel string.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
pub struct FrameClaim {
    pub lane: &'static str,
    pub source: &'static str,
    pub class_ceiling: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub refused: Option<&'static str>,
}

/// Look up a lane. Unknown names are a programmer error — the table is
/// closed.
pub fn frame_for(lane: &str) -> Option<&'static Frame> {
    FRAMES.iter().find(|f| f.lane == lane)
}

/// Derive the claim for `lane` at either the working tree (`rev` absent)
/// or an off-checkout ref.
///
/// Absent-ref is the working tree: source `working_tree`, ceiling `exact`,
/// never refused. A present ref uses the table row as-is; a refused source
/// or refused ceiling surfaces `why` on `refused` so the caller can say so
/// instead of guessing.
pub fn claim(lane: &str, working_tree: bool) -> FrameClaim {
    let row = frame_for(lane).unwrap_or_else(|| {
        panic!("unknown frame lane {lane:?} — add it to FRAMES or pick a real one")
    });
    if working_tree {
        return FrameClaim {
            lane: row.lane,
            source: FrameSource::WorkingTree.as_str(),
            class_ceiling: OffHeadClass::Exact.as_str(),
            refused: None,
        };
    }
    let refused = if row.source == FrameSource::Refused || row.off_head == OffHeadClass::Refused {
        Some(row.why)
    } else {
        None
    };
    FrameClaim {
        lane: row.lane,
        source: row.source.as_str(),
        class_ceiling: row.off_head.as_str(),
        refused,
    }
}

/// `true` when no `?ref=` was supplied (or it was blank). `HEAD` as an
/// explicit ref is still an ODB read — it is not the dirty working tree.
pub fn is_working_tree_rev(rev: Option<&str>) -> bool {
    rev.map(str::trim).filter(|s| !s.is_empty()).is_none()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The CI golden. Lives under `tests/` (where a reviewer looks for a
    /// fixture) and is read from here (where the only test that can
    /// regenerate it lives) — `syntax.rs`'s `PARITY_GOLDEN` convention.
    const FRAMES_GOLDEN: &str = include_str!("../tests/fixtures/frames.golden.json");

    #[test]
    fn the_frame_table_matches_the_checked_in_golden() {
        let actual = serde_json::to_string_pretty(&frames_response()).expect("serialize");
        assert_eq!(
            actual.trim_end(),
            FRAMES_GOLDEN.trim_end(),
            "the D14 frame table differs from tests/fixtures/frames.golden.json.\n\
             Every off-HEAD banner in the reader derives from these bytes, so a change here \
             changes what the instrument CLAIMS. If it is intended, replace the golden with \
             the JSON below (this is exactly `kb-code frames --json`):\n{actual}"
        );
    }

    #[test]
    fn every_lane_is_named_once_and_carries_a_reason() {
        let mut seen: Vec<&str> = Vec::new();
        for f in FRAMES {
            assert!(
                !seen.contains(&f.lane),
                "lane {:?} appears twice in FRAMES — one lane, one frame",
                f.lane
            );
            seen.push(f.lane);
            assert!(
                f.why.len() > 40,
                "lane {:?} has no real reason; an unexplained degrade is not a map",
                f.lane
            );
        }
        assert!(!FRAMES.is_empty());
    }

    /// A refused SOURCE and a non-refused CLASS (or the reverse) would be
    /// a table that promises an answer it never produces — the v7.0
    /// dead-surface defect in a wire contract.
    #[test]
    fn a_refused_source_refuses_its_class() {
        for f in FRAMES {
            if f.source == FrameSource::Refused {
                assert_eq!(
                    f.off_head,
                    OffHeadClass::Refused,
                    "lane {:?} refuses to read anything but claims a class off HEAD",
                    f.lane
                );
            }
        }
    }

    /// D14's own sentence, asserted: the three lanes it names as working
    /// tree are working tree, the two it names as ODB are ODB, blame is
    /// git, usages is capped, lsp-live is refused. This test is the
    /// design-of-record cross-check the golden cannot make on its own (a
    /// golden happily pins a wrong table).
    #[test]
    fn the_table_says_what_d14_says() {
        let by = |lane: &str| FRAMES.iter().find(|f| f.lane == lane).expect(lane);
        for lane in ["text", "symbols", "files"] {
            assert_eq!(by(lane).source, FrameSource::WorkingTree, "{lane}");
        }
        assert_eq!(by("tree").source, FrameSource::Odb);
        assert_eq!(by("file_at_ref").source, FrameSource::Odb);
        assert_eq!(by("blame").source, FrameSource::Git);
        assert!(matches!(
            by("usages").off_head,
            OffHeadClass::Likely | OffHeadClass::Candidate
        ));
        assert_eq!(by("lsp_live").source, FrameSource::Refused);
    }

    #[test]
    fn the_declared_vocabularies_cover_every_row() {
        let r = frames_response();
        for f in FRAMES {
            assert!(r.sources.contains(&f.source.as_str()), "{:?}", f.lane);
            assert!(r.classes.contains(&f.off_head.as_str()), "{:?}", f.lane);
        }
    }

    #[test]
    fn a_working_tree_claim_is_exact_and_never_refused() {
        for f in FRAMES {
            let c = claim(f.lane, true);
            assert_eq!(c.lane, f.lane);
            assert_eq!(c.source, "working_tree");
            assert_eq!(c.class_ceiling, "exact");
            assert_eq!(c.refused, None);
        }
    }

    #[test]
    fn an_off_head_claim_copies_the_table_and_surfaces_a_refusal() {
        let file = claim("file_at_ref", false);
        assert_eq!(file.source, "odb");
        assert_eq!(file.class_ceiling, "exact");
        assert_eq!(file.refused, None);

        let lsp = claim("lsp_live", false);
        assert_eq!(lsp.source, "refused");
        assert_eq!(lsp.class_ceiling, "refused");
        assert!(lsp.refused.is_some_and(|w| w.contains("kb-lip")));

        let symbols = claim("symbols", false);
        assert_eq!(symbols.source, "working_tree");
        assert_eq!(symbols.class_ceiling, "refused");
        assert!(symbols.refused.is_some());
    }

    #[test]
    fn blank_or_absent_rev_is_the_working_tree() {
        assert!(is_working_tree_rev(None));
        assert!(is_working_tree_rev(Some("")));
        assert!(is_working_tree_rev(Some("  ")));
        assert!(!is_working_tree_rev(Some("HEAD")));
        assert!(!is_working_tree_rev(Some("main")));
    }
}
