import type { ReadingSection } from "../api/generated/ReadingSection";
import type { ReadingSectionBeacon } from "../api/generated/ReadingSectionBeacon";
import type { ReadingSummary } from "../api/generated/ReadingSummary";
import type { SectionState } from "../api/generated/SectionState";

// R1 — reading model, ported VERBATIM from crates/kb-core/src/reading.rs (the
// source of truth). Keep in lock-step; reading.test.ts mirrors the Rust #[test]
// fixtures (expected_ms_floors_and_scales, classify_unseen_skim_read) — the same
// dual-grammar discipline invariants #25/#29 mandate. Rust `as i64` truncates
// toward zero, so we use Math.trunc (NOT Math.round) to match bit-for-bit.
export const WORDS_PER_MINUTE = 200;
export const READ_FRACTION = 0.5;
export const MIN_EXPECTED_MS = 500;
export const FULLY_READ_PCT = 95;

/** Expected active-reading time (ms) for `words` at WPM, floored. */
export function expectedMs(words: number): number {
  if (words <= 0) return MIN_EXPECTED_MS;
  const ms = Math.trunc((words / WORDS_PER_MINUTE) * 60_000);
  return Math.max(ms, MIN_EXPECTED_MS);
}

/** Pure classifier. enters<=0 ⇒ unseen; dwell ≥ 0.5×expected ⇒ read, else skim. */
export function classify(
  dwellMs: number,
  words: number,
  enters: number,
): SectionState {
  if (enters <= 0) return "unseen";
  const threshold = Math.trunc(expectedMs(words) * READ_FRACTION);
  return dwellMs >= threshold ? "read" : "skim";
}

// R1 — build a SINGLE-VISIT ReadingSummary from the latest live `kb:reading`
// beacon, for optimistic UI only. This is intentionally NOT a port of reading.rs
// `summarize` (which merges across visits); it's a one-visit reduction over the
// current beacon. The server summary stays the cross-visit truth and re-asserts
// on the next history.recorded — so callers should max() these percentages
// against the server's, never let the live single-visit value pull them down.
export function liveSummaryFromBeacon(
  beacons: ReadingSectionBeacon[],
  activeMs: number,
  completionPct: number,
): ReadingSummary {
  const sections: ReadingSection[] = beacons.map((b) => ({
    section_id: b.id,
    section_idx: b.idx,
    text: b.text,
    level: b.level,
    words: b.words,
    dwell_ms: b.dwell_ms,
    enters: b.enters,
    state: classify(b.dwell_ms, b.words, b.enters),
  }));
  const totalWords = sections.reduce((s, x) => s + x.words, 0);
  const readWords = sections
    .filter((x) => x.state === "read")
    .reduce((s, x) => s + x.words, 0);
  const readPct =
    totalWords > 0 ? Math.round((readWords / totalWords) * 100) : 0;
  const topSections = [...sections]
    .filter((x) => x.dwell_ms > 0)
    .sort((a, b) => b.dwell_ms - a.dwell_ms)
    .map((x) => x.section_id);
  return {
    completion_pct: completionPct,
    read_pct: readPct,
    is_fully_read: completionPct >= FULLY_READ_PCT,
    active_ms_total: activeMs,
    visit_count: 1,
    first_read_at: null,
    last_read_at: null,
    stopped_at: null,
    sections,
    top_sections: topSections,
  };
}
