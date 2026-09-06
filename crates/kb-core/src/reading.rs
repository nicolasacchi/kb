//! Reading-progress classification + summary (RP-track). A "reading" is what
//! the daemon records while a human scrolls an artifact in the SPA: per
//! heading-delimited section, the accumulated active dwell time and how many
//! times the viewport entered it (see `migrations/V0014`). Everything here is
//! pure, side-effect-free logic the server route + CLI drive:
//!
//! - **classify** — turn a section's `(dwell_ms, words, enters)` into
//!   [`SectionState::Read`] / `Skim` / `Unseen`. The signal that separates
//!   "read" from "skimmed past" is dwell time relative to the section's
//!   *expected* reading time (its word count at ~200 wpm) — the
//!   scroll-speed intuition, made concrete.
//! - **summarize** — merge a single artifact's section rows across all its
//!   visits into a [`ReadingSummary`] (completion %, words-weighted
//!   read %, active time, stop-point, per-section breakdown, interest
//!   ranking) for `GET …/reading`, `kb reading`, and the SPA heatmap.
//!
//! The capture side persists the FULL table-of-contents per visit (every
//! heading, even ones never reached, at `dwell_ms = enters = 0`), so a
//! never-reached section is a real row that classifies to `Unseen` — that's
//! what lets the summary report coverage, not just the sections that were
//! touched. `section_id` is the runtime's live DOM heading id, shared with
//! the TOC mini-spy so the SPA heatmap joins on it.

use serde::Serialize;

/// Words-per-minute baseline for the expected-reading-time estimate. A
/// deliberately middling silent-reading speed; the classifier only needs to
/// separate "lingered" from "scrolled past", not measure WPM precisely.
pub const WORDS_PER_MINUTE: i64 = 200;
/// A section counts as Read once dwell reaches this fraction of its expected
/// reading time. 0.5 ⇒ "spent at least half the time a full read would take".
pub const READ_FRACTION: f64 = 0.5;
/// Floor on expected reading time so a heading-only / wordless section still
/// requires a beat of dwell before it classifies Read (avoids 0ms→Read).
pub const MIN_EXPECTED_MS: i64 = 500;
/// `completion_pct` at or above this counts the artifact fully read (matches
/// the SPA reading-chip's existing 95% threshold).
pub const FULLY_READ_PCT: u8 = 95;
/// How many sections the interest ranking (`top_sections`) returns.
pub const TOP_SECTIONS: usize = 5;

/// Coarse per-artifact reading rollup, batched across the whole `history`
/// table for search filtering + sort (Q-track). Built from the *newest*
/// open visit's scroll completion — deliberately cheaper than a full
/// [`ReadingSummary`] (no per-section `reading_sections` work), matching
/// the scroll-based signal the SPA reading-chip already shows. A list
/// `read_override` is overlaid on top (override wins, mirroring
/// [`crate::lists::derive_read_state`]). Computed read-only over the
/// `history` and `list_entries` tables, so it never mutates storage or
/// bumps the index generation (root invariants #8 / #15 / #19).
#[derive(Debug, Clone)]
pub struct ReadRollup {
    /// `started_at` of the most-recent open visit; `None` for an artifact
    /// that was never opened but carries a list `read_override`.
    pub last_opened_unix: Option<i64>,
    /// Furthest scroll completion of the latest visit, `0..=100`.
    pub completion_pct: u8,
    /// Whole-artifact read state: the override when set, else derived from
    /// `completion_pct` (`>= FULLY_READ_PCT` ⇒ Read, opened ⇒ InProgress).
    pub state: crate::lists::ReadState,
}

/// How thoroughly the reader engaged with one section.
#[cfg_attr(feature = "ts-export", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum SectionState {
    /// The viewport never entered this section (`enters == 0`).
    Unseen,
    /// Entered, but dwell stayed under the read threshold — scrolled past.
    Skim,
    /// Dwell reached the read threshold for the section's length.
    Read,
}

impl SectionState {
    pub fn as_str(self) -> &'static str {
        match self {
            SectionState::Unseen => "unseen",
            SectionState::Skim => "skim",
            SectionState::Read => "read",
        }
    }
}

/// Expected active-reading time (ms) for a section of `words` words at
/// [`WORDS_PER_MINUTE`], floored at [`MIN_EXPECTED_MS`].
pub fn expected_ms(words: i64) -> i64 {
    if words <= 0 {
        return MIN_EXPECTED_MS;
    }
    let ms = (words as f64 / WORDS_PER_MINUTE as f64 * 60_000.0) as i64;
    ms.max(MIN_EXPECTED_MS)
}

/// Pure classifier. `enters == 0` ⇒ never in viewport ⇒ `Unseen`; otherwise
/// `Read` once `dwell_ms` reaches `READ_FRACTION × expected_ms(words)`, else
/// `Skim`. `content_px` is deliberately NOT an input — words drive expected
/// time; pixels are a heatmap artifact only.
pub fn classify(dwell_ms: i64, words: i64, enters: i64) -> SectionState {
    if enters <= 0 {
        return SectionState::Unseen;
    }
    let threshold = (expected_ms(words) as f64 * READ_FRACTION) as i64;
    if dwell_ms >= threshold {
        SectionState::Read
    } else {
        SectionState::Skim
    }
}

/// One stored `reading_sections` row — the merge input to [`summarize`].
#[derive(Debug, Clone)]
pub struct ReadingSectionRow {
    pub visit_id: i64,
    pub section_id: String,
    pub section_idx: i64,
    pub section_text: String,
    pub level: i64,
    pub words: i64,
    pub content_px: i64,
    pub dwell_ms: i64,
    pub enters: i64,
    pub first_at: i64,
    pub last_at: i64,
}

/// Per-visit roll-up taken from the history `open` row — the other
/// [`summarize`] input. `scroll_y_max`/`scroll_max` give the scroll-depth
/// completion; `active_ms`/`last_section` give active time + the stop-point.
#[derive(Debug, Clone)]
pub struct VisitRollup {
    pub visit_id: i64,
    pub started_at: i64,
    pub updated_at: i64,
    pub scroll_y_max: i64,
    pub scroll_max: i64,
    pub active_ms: i64,
    pub last_section: Option<String>,
}

/// One section in a [`ReadingSummary`], merged across the artifact's visits.
#[cfg_attr(
    feature = "ts-export",
    derive(ts_rs::TS),
    ts(export, rename = "ReadingSection")
)]
#[derive(Debug, Clone, Serialize)]
pub struct SectionSummary {
    pub section_id: String,
    pub section_idx: i64,
    pub text: String,
    pub level: i64,
    pub words: i64,
    pub dwell_ms: i64,
    pub enters: i64,
    pub state: SectionState,
}

/// Where the reader was when they last left the artifact.
#[cfg_attr(
    feature = "ts-export",
    derive(ts_rs::TS),
    ts(export, rename = "ReadingStopPoint")
)]
#[derive(Debug, Clone, Serialize)]
pub struct StopPoint {
    pub section_id: String,
    pub text: String,
    /// Approximate scroll position of the section's top, 0..100 (from
    /// cumulative `content_px`; falls back to ordinal position).
    pub pct: u8,
}

/// Cross-visit reading summary for one artifact.
#[cfg_attr(feature = "ts-export", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Clone, Serialize)]
pub struct ReadingSummary {
    /// Furthest scroll depth reached, max across visits (the "how far"
    /// headline + the `--lite` number).
    pub completion_pct: u8,
    /// Words-weighted fraction of the artifact actually **read** (Read
    /// sections' words / all sections' words) — "how much did they truly
    /// read", distinct from how far they scrolled.
    pub read_pct: u8,
    pub is_fully_read: bool,
    pub active_ms_total: i64,
    pub visit_count: u32,
    pub first_read_at: Option<i64>,
    pub last_read_at: Option<i64>,
    pub stopped_at: Option<StopPoint>,
    /// Per-section breakdown in document order. Empty in the `lite` view.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    #[cfg_attr(
        feature = "ts-export",
        ts(as = "Option<Vec<SectionSummary>>", optional)
    )]
    pub sections: Vec<SectionSummary>,
    /// Section ids by descending dwell (interest ranking); excludes
    /// zero-dwell sections.
    pub top_sections: Vec<String>,
}

fn pct(numer: i64, denom: i64) -> u8 {
    if denom <= 0 {
        return 0;
    }
    let p = (numer as f64 / denom as f64 * 100.0).round();
    p.clamp(0.0, 100.0) as u8
}

/// Merge an artifact's section rows (across all visits) + per-visit rollups
/// into a [`ReadingSummary`]. `lite` skips the per-section breakdown +
/// interest ranking (whole-page numbers only) — the merge is otherwise the
/// same. Pure: no clocks, no I/O.
pub fn summarize(
    sections: &[ReadingSectionRow],
    visits: &[VisitRollup],
    lite: bool,
) -> ReadingSummary {
    use std::collections::HashMap;

    // --- per-visit headline numbers -------------------------------------
    let completion_pct = visits
        .iter()
        .map(|v| pct(v.scroll_y_max, v.scroll_max))
        .max()
        .unwrap_or(0);
    let active_ms_total: i64 = visits.iter().map(|v| v.active_ms).sum();
    let visit_count = visits.len() as u32;
    let first_read_at = visits.iter().map(|v| v.started_at).min();
    let last_read_at = visits.iter().map(|v| v.updated_at).max();

    // --- merge sections by id across visits -----------------------------
    // Preserve first-seen order, then sort by section_idx. Latest row (by
    // last_at) wins for the descriptive fields; dwell + enters sum;
    // content_px (a capture artifact kept off SectionSummary) is max-merged
    // into a side map for the stop-point position only.
    let mut order: Vec<String> = Vec::new();
    let mut merged: HashMap<String, SectionSummary> = HashMap::new();
    let mut latest_at: HashMap<String, i64> = HashMap::new();
    let mut px_by_id: HashMap<String, i64> = HashMap::new();
    for r in sections {
        let entry = merged.entry(r.section_id.clone()).or_insert_with(|| {
            order.push(r.section_id.clone());
            SectionSummary {
                section_id: r.section_id.clone(),
                section_idx: r.section_idx,
                text: r.section_text.clone(),
                level: r.level,
                words: r.words,
                dwell_ms: 0,
                enters: 0,
                state: SectionState::Unseen,
            }
        });
        entry.dwell_ms += r.dwell_ms;
        entry.enters += r.enters;
        let px = px_by_id.entry(r.section_id.clone()).or_insert(0);
        *px = (*px).max(r.content_px);
        // Descriptive fields follow the most-recently-observed visit.
        let prev = latest_at.get(&r.section_id).copied().unwrap_or(i64::MIN);
        if r.last_at >= prev {
            latest_at.insert(r.section_id.clone(), r.last_at);
            entry.section_idx = r.section_idx;
            entry.text = r.section_text.clone();
            entry.level = r.level;
            entry.words = r.words;
        }
    }
    let mut merged: Vec<SectionSummary> = order
        .into_iter()
        .filter_map(|id| merged.remove(&id))
        .collect();
    for s in &mut merged {
        s.state = classify(s.dwell_ms, s.words, s.enters);
    }
    merged.sort_by_key(|s| (s.section_idx, s.section_id.clone()));

    // --- words-weighted read fraction -----------------------------------
    let total_words: i64 = merged.iter().map(|s| s.words.max(0)).sum();
    let read_words: i64 = merged
        .iter()
        .filter(|s| s.state == SectionState::Read)
        .map(|s| s.words.max(0))
        .sum();
    let read_pct = if total_words > 0 {
        pct(read_words, total_words)
    } else {
        // No word estimates — fall back to a section-count fraction.
        let total = merged.len() as i64;
        let read = merged
            .iter()
            .filter(|s| s.state == SectionState::Read)
            .count() as i64;
        pct(read, total)
    };

    // --- stop-point: the most-recent visit's last_section ----------------
    let stopped_at = visits
        .iter()
        .max_by_key(|v| (v.started_at, v.visit_id))
        .and_then(|v| v.last_section.clone())
        .and_then(|sid| {
            let idx = merged.iter().position(|s| s.section_id == sid)?;
            // pct = top of the stop section / total height (cumulative px),
            // falling back to ordinal position when px is unavailable.
            let total_px: i64 = px_by_id.values().sum();
            let stop_pct = if total_px > 0 {
                let before: i64 = merged
                    .iter()
                    .take(idx)
                    .map(|s| px_by_id.get(&s.section_id).copied().unwrap_or(0))
                    .sum();
                pct(before, total_px)
            } else if merged.len() > 1 {
                pct(idx as i64, (merged.len() - 1) as i64)
            } else {
                0
            };
            Some(StopPoint {
                section_id: merged[idx].section_id.clone(),
                text: merged[idx].text.clone(),
                pct: stop_pct,
            })
        });

    // --- interest ranking by dwell --------------------------------------
    let top_sections = if lite {
        Vec::new()
    } else {
        let mut by_dwell: Vec<&SectionSummary> = merged.iter().filter(|s| s.dwell_ms > 0).collect();
        by_dwell.sort_by(|a, b| {
            b.dwell_ms
                .cmp(&a.dwell_ms)
                .then(a.section_idx.cmp(&b.section_idx))
        });
        by_dwell
            .into_iter()
            .take(TOP_SECTIONS)
            .map(|s| s.section_id.clone())
            .collect()
    };

    ReadingSummary {
        completion_pct,
        read_pct,
        is_fully_read: completion_pct >= FULLY_READ_PCT,
        active_ms_total,
        visit_count,
        first_read_at,
        last_read_at,
        stopped_at,
        sections: if lite { Vec::new() } else { merged },
        top_sections,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(id: &str, idx: i64, words: i64, dwell: i64, enters: i64) -> ReadingSectionRow {
        ReadingSectionRow {
            visit_id: 1,
            section_id: id.to_string(),
            section_idx: idx,
            section_text: id.to_string(),
            level: 2,
            words,
            content_px: words * 5, // rough proxy so cumulative-px paths run
            dwell_ms: dwell,
            enters,
            first_at: 1000,
            last_at: 1000 + dwell / 1000,
        }
    }

    fn visit(
        id: i64,
        started: i64,
        ymax: i64,
        smax: i64,
        active: i64,
        last: Option<&str>,
    ) -> VisitRollup {
        VisitRollup {
            visit_id: id,
            started_at: started,
            updated_at: started + active / 1000,
            scroll_y_max: ymax,
            scroll_max: smax,
            active_ms: active,
            last_section: last.map(str::to_string),
        }
    }

    #[test]
    fn expected_ms_floors_and_scales() {
        assert_eq!(expected_ms(0), MIN_EXPECTED_MS);
        assert_eq!(expected_ms(-5), MIN_EXPECTED_MS);
        assert_eq!(expected_ms(200), 60_000); // 200 words @ 200wpm = 1 min
        assert_eq!(expected_ms(100), 30_000);
        assert!(expected_ms(1) >= MIN_EXPECTED_MS); // a 1-word section still floors
    }

    // invariant:19 dwell-classifier
    #[test]
    fn classify_unseen_skim_read() {
        assert_eq!(classify(0, 200, 0), SectionState::Unseen); // never entered
        assert_eq!(classify(50_000, 200, 0), SectionState::Unseen); // enters gate wins
        assert_eq!(classify(0, 200, 1), SectionState::Skim); // entered, no dwell
        assert_eq!(classify(29_999, 200, 1), SectionState::Skim); // just under 0.5×60s
        assert_eq!(classify(30_000, 200, 1), SectionState::Read); // exactly 0.5×expected
        assert_eq!(classify(60_000, 200, 1), SectionState::Read);
        // wordless heading: 0.5 × MIN_EXPECTED_MS = 250ms threshold
        assert_eq!(classify(249, 0, 1), SectionState::Skim);
        assert_eq!(classify(250, 0, 1), SectionState::Read);
    }

    #[test]
    fn summarize_empty_is_zero() {
        let s = summarize(&[], &[], false);
        assert_eq!(s.completion_pct, 0);
        assert_eq!(s.read_pct, 0);
        assert!(!s.is_fully_read);
        assert_eq!(s.visit_count, 0);
        assert!(s.stopped_at.is_none());
        assert!(s.sections.is_empty());
        assert!(s.top_sections.is_empty());
        assert_eq!(s.first_read_at, None);
    }

    #[test]
    fn summarize_single_visit_classifies_and_ranks() {
        let rows = vec![
            row("intro", 0, 200, 60_000, 1),  // read (full)
            row("middle", 1, 200, 40_000, 2), // read (>= 30s), most dwell after intro? 40k<60k
            row("risks", 2, 200, 5_000, 1),   // skim
            row("appendix", 3, 200, 0, 0),    // unseen (entered 0)
        ];
        let visits = vec![visit(1, 5_000, 620, 1000, 105_000, Some("risks"))];
        let s = summarize(&rows, &visits, false);

        assert_eq!(s.visit_count, 1);
        assert_eq!(s.completion_pct, 62); // 620/1000
        assert_eq!(s.active_ms_total, 105_000);
        // sections in document order
        let states: Vec<_> = s.sections.iter().map(|x| x.state).collect();
        assert_eq!(
            states,
            vec![
                SectionState::Read,
                SectionState::Read,
                SectionState::Skim,
                SectionState::Unseen
            ]
        );
        // read_pct words-weighted: read words 400 / total 800 = 50
        assert_eq!(s.read_pct, 50);
        // interest: intro (60k) before middle (40k); skim/unseen lower
        assert_eq!(s.top_sections.first().map(String::as_str), Some("intro"));
        assert_eq!(s.top_sections.get(1).map(String::as_str), Some("middle"));
        assert!(!s.top_sections.iter().any(|id| id == "appendix")); // zero-dwell excluded
                                                                    // stop-point is the most-recent visit's last_section
        let sp = s.stopped_at.expect("stop point");
        assert_eq!(sp.section_id, "risks");
    }

    #[test]
    fn summarize_merges_dwell_across_visits() {
        // visit 1 only skims "risks"; visit 2 finishes it → merged = Read.
        let rows = vec![
            row("risks", 0, 200, 10_000, 1), // visit 1 partial
            ReadingSectionRow {
                visit_id: 2,
                last_at: 9999,
                dwell_ms: 25_000,
                ..row("risks", 0, 200, 25_000, 1)
            },
        ];
        let visits = vec![
            visit(1, 1_000, 500, 1000, 12_000, Some("risks")),
            visit(2, 9_000, 1000, 1000, 30_000, Some("risks")),
        ];
        let s = summarize(&rows, &visits, false);
        assert_eq!(s.visit_count, 2);
        assert_eq!(s.completion_pct, 100); // visit 2 reached the bottom
        assert!(s.is_fully_read);
        assert_eq!(s.active_ms_total, 42_000);
        // 10k + 25k = 35k >= 30k threshold → Read
        assert_eq!(s.sections.len(), 1);
        assert_eq!(s.sections[0].state, SectionState::Read);
        assert_eq!(s.sections[0].dwell_ms, 35_000);
        assert_eq!(s.sections[0].enters, 2);
    }

    #[test]
    fn summarize_lite_omits_sections() {
        let rows = vec![row("a", 0, 200, 60_000, 1)];
        let visits = vec![visit(1, 1, 1000, 1000, 60_000, Some("a"))];
        let s = summarize(&rows, &visits, true);
        assert!(s.sections.is_empty());
        assert!(s.top_sections.is_empty());
        // whole-page numbers still computed
        assert_eq!(s.completion_pct, 100);
        assert_eq!(s.visit_count, 1);
    }
}
