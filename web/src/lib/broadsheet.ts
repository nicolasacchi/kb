// W2.5 — the broadsheet home: pure, colocated-tested composition for the
// gallery's fifth view (`?view=press`). Packs the CURRENT filter set
// (whatever `rows` the caller already filtered — see gallery.tsx's
// `filtered`) into a deterministic front page: one lead story, four
// features, ~8 one-line briefs, and a "continuing coverage" rail. Every
// ranking here is client-side and LLM-free (no in-daemon LLM, no new
// endpoint) — same posture as lib/lobby.ts's `pickHighlights`.
//
// Side-effect-free: no fetches, no Date.now() calls without an injectable
// `now`/`nowMs` parameter (tests pin exact timestamps).

import type { DocSummary } from "../api/client";
import type { InboxItem } from "../api/inbox";
import { sumWords } from "./lobby";

// ── 1. comment mass (open-comment counts per artifact) ────────────────────

/// Aggregate open-comment inbox rows into a per-artifact count. Absent from
/// the map == zero open comments (the caller's `commentMass.get(id) ?? 0`
/// convention). Pure — the caller feeds this the ONE `fetchInbox({kb})`
/// response; a failed/empty fetch already resolves to `{items: []}`
/// (api/inbox.ts's EMPTY fallback), so this naturally yields an empty map.
export function aggregateOpenCounts(items: InboxItem[]): Map<string, number> {
  const m = new Map<string, number>();
  for (const it of items) {
    m.set(it.artifact_id, (m.get(it.artifact_id) ?? 0) + 1);
  }
  return m;
}

// ── 2. lead/feature scoring ────────────────────────────────────────────────

const RECENCY_HALF_LIFE_DAYS = 30;
const MAX_COMMENT_BONUS = 4;
const DAY_MS = 86_400_000;

/// Exponential half-life decay on an artifact's `mtime_unix` (unix seconds).
/// `nowMs` is the caller's clock (ms, `Date.now()`-shaped so callers never
/// have to remember a seconds/ms split at the call site). Missing mtime ⇒ 0
/// (can't score recency — deterministic tie-break on id still applies at
/// the composePage level, so this never produces a random ordering). A
/// future mtime (clock skew) clamps to age 0 (decay 1), never > 1.
export function recencyDecay(
  mtimeUnix: number | null | undefined,
  nowMs: number,
): number {
  if (mtimeUnix == null) return 0;
  const ageDays = Math.max(0, (nowMs - mtimeUnix * 1000) / DAY_MS);
  return Math.pow(0.5, ageDays / RECENCY_HALF_LIFE_DAYS);
}

/// The decomposed lead/feature score: `wordFactor × recencyFactor ×
/// commentFactor`. Every factor is retained (not just the product) so the
/// UI can render a `ScoreExplain` chip (components/ScoreExplain.tsx) — the
/// house explainability move already used by the resurface strip.
export type ScoreExplain = {
  wordFactor: number;
  recencyFactor: number;
  commentFactor: number;
  openComments: number;
  score: number;
};

/// `ln(1 + word_count) × recency_decay(mtime, 30d half-life) × (1 +
/// min(open_comments, 4)/4)`. Missing `word_count` counts as 0 (a
/// zero-word article scores 0 — it still sorts, just last, via the
/// deterministic id tiebreak in `composePage`).
export function scoreDoc(
  doc: DocSummary,
  openComments: number,
  nowMs: number,
): ScoreExplain {
  const wordFactor = Math.log(1 + Math.max(0, doc.word_count ?? 0));
  const recencyFactor = recencyDecay(doc.mtime_unix, nowMs);
  const commentFactor = 1 + Math.min(openComments, MAX_COMMENT_BONUS) / MAX_COMMENT_BONUS;
  return {
    wordFactor,
    recencyFactor,
    commentFactor,
    openComments,
    score: wordFactor * recencyFactor * commentFactor,
  };
}

// ── 3. page composition ────────────────────────────────────────────────────

const FEATURES_N = 4;
const BRIEFS_N = 8;
const COVERAGE_N = 6;

export type LeadStory = {
  doc: DocSummary;
  explain: ScoreExplain;
};

export type BroadsheetPage = {
  /// `null` only when `rows` is empty — every other section degrades to
  /// `[]` (skip-if-empty is the caller's job; this just never invents rows).
  lead: LeadStory | null;
  features: DocSummary[];
  briefs: DocSummary[];
  /// Rows whose mtime moved past the reader's last visit (the W1 "updated
  /// since read" corner-mark rule, `Card.tsx`'s `updatedSinceRead`),
  /// newest-first, capped at 6.
  coverage: DocSummary[];
};

/// Deterministic front-page composition over the CURRENT filter set — the
/// broadsheet is a projection of whatever `rows` the caller already
/// filtered, never a re-query. Total order: score desc, id asc (mirrors
/// `pickHighlights`'s tiebreak convention) — a page reload with unchanged
/// inputs always renders byte-identical sections.
export function composePage(
  rows: DocSummary[],
  commentMass: Map<string, number>,
  nowMs: number = Date.now(),
): BroadsheetPage {
  const ranked = rows
    .map((doc) => ({
      doc,
      explain: scoreDoc(doc, commentMass.get(doc.id) ?? 0, nowMs),
    }))
    .sort((a, b) => {
      const d = b.explain.score - a.explain.score;
      if (d !== 0) return d;
      return a.doc.id.localeCompare(b.doc.id);
    });

  const lead = ranked.length > 0 ? ranked[0] : null;
  const features = ranked.slice(1, 1 + FEATURES_N).map((r) => r.doc);
  const briefs = ranked
    .slice(1 + FEATURES_N, 1 + FEATURES_N + BRIEFS_N)
    .map((r) => r.doc);

  const coverage = rows
    .filter(
      (d) =>
        d.last_opened_unix != null &&
        d.mtime_unix != null &&
        d.mtime_unix > d.last_opened_unix,
    )
    .sort((a, b) => {
      const d = (b.mtime_unix ?? 0) - (a.mtime_unix ?? 0);
      if (d !== 0) return d;
      return a.id.localeCompare(b.id);
    })
    .slice(0, COVERAGE_N);

  return { lead, features, briefs, coverage };
}

// ── 4. masthead — "Vol. YYYY · No. WW" + census line ───────────────────────

/// ISO-8601 week number + ISO week-year for a LOCAL calendar date (mirrors
/// `lib/time.ts`'s `dayBucket` — local-tz, not UTC — so "this week" agrees
/// with the reader's wall clock). Thursday-anchored: shift to the Thursday
/// of the same ISO week, then count from that ISO year's January 1st. A
/// year-end/year-start date can therefore belong to the OTHER calendar
/// year's ISO week 1 or 52/53 — the golden tests below pin the classic
/// boundary cases (2015/2016, 2020/2021, 2025/2026/2027).
export function isoWeekInfo(d: Date): { isoYear: number; week: number } {
  const date = new Date(d.getFullYear(), d.getMonth(), d.getDate());
  const isoDayNum = date.getDay() || 7; // Mon=1 .. Sun=7
  date.setDate(date.getDate() + 4 - isoDayNum); // Thursday of this ISO week
  const isoYear = date.getFullYear();
  const yearStart = new Date(isoYear, 0, 1);
  const diffDays = Math.round((date.getTime() - yearStart.getTime()) / DAY_MS);
  const week = Math.ceil((diffDays + 1) / 7);
  return { isoYear, week };
}

/// "Vol. 2026 · No. 03" — the masthead's issue line. Week is always
/// zero-padded to 2 digits (weeks run 1..53).
export function mastheadVolume(nowMs: number = Date.now()): string {
  const { isoYear, week } = isoWeekInfo(new Date(nowMs));
  return `Vol. ${isoYear} · No. ${String(week).padStart(2, "0")}`;
}

/// "N artifact(s) [of M] · N word(s)" — `docCount` (the kb's corpus-wide
/// `KbSummary.doc_count`) is shown as "of M" context only when it differs
/// from `rows.length` (an unfiltered issue has nothing to disambiguate).
/// Word sum reuses `lib/lobby.ts`'s `sumWords` (same documented
/// undercount-on-pagination caveat — this is the loaded page, not a
/// corpus-wide scan).
export function censusLine(
  rows: DocSummary[],
  docCount?: number | null,
): string {
  const n = rows.length;
  const words = sumWords(rows);
  const showOf = docCount != null && docCount !== n;
  const ofPart = showOf ? ` of ${(docCount as number).toLocaleString()}` : "";
  // "N of M artifacts" pluralizes on M (the set being related to), not N —
  // "1 of 40 artifact" reads wrong; "1 of 40 artifacts" is the natural form.
  const artifactBasis = showOf ? (docCount as number) : n;
  return `${n.toLocaleString()}${ofPart} artifact${artifactBasis === 1 ? "" : "s"} · ${words.toLocaleString()} word${words === 1 ? "" : "s"}`;
}
