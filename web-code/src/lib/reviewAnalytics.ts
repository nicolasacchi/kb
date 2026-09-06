// PRR-U8 (design-addendum-2.md §C) — pure formatting/grouping helpers for
// the Review Room landing's Analytics section
// (`components/reviews/AnalyticsSection.tsx`). Deterministic, no I/O — the
// daemon-side `review_analytics.rs` already did every aggregation; this
// module only renders what it returned, honestly: a `null` rate/latency is
// ALWAYS an em-dash, never a fabricated `0`/`0%` (the addendum's own "no
// fabricated 0%" law, mirroring `lib/attentionRamp.ts`'s `formatAttentionScore`
// / `RiskBadge.tsx`'s null → "—" convention already established elsewhere
// in this crate).
import type { AnalyticsSeverityDispositionCell } from "../api/types";

/** `null` → "—" (never "0%"). */
export function formatAnalyticsRate(rate: number | null): string {
  if (rate == null) return "—";
  return `${Math.round(rate * 100)}%`;
}

/**
 * `null` → "—" (never "0s"). Coarse, human buckets — this is a landing-page
 * stat tile (median/p90 time-to-disposition), not a precision timer.
 */
export function formatAnalyticsLatency(secs: number | null): string {
  if (secs == null) return "—";
  if (secs < 0) return "—";
  if (secs < 60) return `${secs}s`;
  const mins = Math.round(secs / 60);
  if (mins < 60) return `${mins}m`;
  const hours = Math.round(secs / 3600);
  if (hours < 48) return `${hours}h`;
  const days = Math.round(secs / 86_400);
  return `${days}d`;
}

/**
 * Group `by_severity_disposition`'s flat cell list by severity, preserving
 * the server's own per-severity cell order (`review_analytics.rs`'s
 * `disposition_labels()`'s doc: agree, dispute, waive, fix-later,
 * undecided) — this fn never re-sorts, only buckets.
 */
export function groupBySeverity(
  cells: readonly AnalyticsSeverityDispositionCell[],
): Map<string, AnalyticsSeverityDispositionCell[]> {
  const m = new Map<string, AnalyticsSeverityDispositionCell[]>();
  for (const c of cells) {
    const list = m.get(c.severity);
    if (list) list.push(c);
    else m.set(c.severity, [c]);
  }
  return m;
}

export interface DispositionSegment {
  disposition: string;
  count: number;
  /** 0..100, percentage of THIS ROW's own total (never the corpus total). */
  pct: number;
}

/**
 * CSS-bar segment widths for one severity's disposition row. Zero-count
 * cells still get a `pct: 0` segment (never omitted) so the bar's own DOM
 * shape stays stable across a live refetch. An all-zero row → every
 * segment `0` (never `NaN`/`Infinity` from a 0/0 divide).
 */
export function dispositionSegments(
  cells: readonly AnalyticsSeverityDispositionCell[],
): DispositionSegment[] {
  const total = cells.reduce((s, c) => s + c.count, 0);
  return cells.map((c) => ({
    disposition: c.disposition,
    count: c.count,
    pct: total > 0 ? (c.count / total) * 100 : 0,
  }));
}
