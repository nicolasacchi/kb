// V76-R2a — the Review Room's pure half: chip/icon mappings, section
// decorators, hero derivations and truncation helpers. Every component in
// `components/reviews/` RENDERS what these functions return; none of them
// re-derives a count, a colour or an icon (the kbc-tree/1 rule — "do not
// re-derive a count, an aggregate or a rank" — applied to the Room).
//
// Token discipline: every `token` below is the NAME of an existing
// kbc-theme/1 custom property (`tokens.css` / `themes.gen.css`), never a
// raw hue — `npm run lint:themes` stays the gate. Every `icon` is a key of
// `components/icons.tsx`'s `Icon` map; `reviewRoom.test.ts` pins BOTH
// sides of that (a mapped icon that does not exist fails the golden, and
// the mapping tables themselves are golden-pinned).

import type { FindingSeverity, ReviewFileRow, ReviewFinding, ReviewReport } from "../api/types";

// ── the live counts ───────────────────────────────────────────────────────
// `liveFindingCounts` moved HERE from `ReportPanel.tsx` in V76-R2a so the
// hero, the panel and the rail can all derive from ONE home without a
// component↔lib import cycle. `ReportPanel` re-exports it (its unit test's
// import path is unchanged). Stat tiles are ALWAYS derived from live
// finding rows (PRR-U2 §8) — never from the report's own claimed `stats`.

export interface LiveCounts {
  blockers: number;
  concerns: number;
  verified: number;
}

export function liveFindingCounts(findings: ReviewFinding[]): LiveCounts {
  const counts: LiveCounts = { blockers: 0, concerns: 0, verified: 0 };
  for (const f of findings) {
    if (f.superseded) continue;
    if (f.severity === "blocker") counts.blockers += 1;
    else if (f.severity === "concern") counts.concerns += 1;
    else counts.verified += 1;
  }
  return counts;
}

/// An icon name from the ONE icon set (`components/icons.tsx`). Typed as a
/// plain `string` here so this module stays JSX-free; the golden test
/// asserts every mapped name is a real `keyof typeof Icon`.
export interface ChipSpec {
  /// The custom-property NAME (e.g. `"--red"`) — consumers set
  /// `style={{ "--kbc-room-chip-color": `var(${token})` }}` so the chip's
  /// wash, border and glyph all key off ONE token.
  token: string;
  icon: string;
  /// Short accessible label when the chip's text alone is ambiguous.
  title?: string;
}

// ── severity chips ────────────────────────────────────────────────────────
// The server's closed 3-value vocabulary (`store::is_valid_severity`).
// Severity IS a verdict axis, so it gets the three verdict hues — the same
// assignment `AgentVerdictCard`'s `severityToken` already makes, kept in
// lock-step deliberately.

export const SEVERITY_CHIPS: Record<FindingSeverity, ChipSpec> = {
  blocker: { token: "--red", icon: "Warn" },
  concern: { token: "--warn", icon: "Warn" },
  ok: { token: "--green", icon: "Check" },
};

export function severityChip(severity: FindingSeverity): ChipSpec {
  return SEVERITY_CHIPS[severity] ?? { token: "--ink-dim", icon: "Dot" };
}

// ── act chips ─────────────────────────────────────────────────────────────
// findings v2's speech-act axis (`FindingCard.tsx`'s `findingAct`). `act` is
// a FREE string on the wire (an older daemon omits it; a newer one may add
// an act this build does not know) — the eight known acts get their own
// icon + token, and EVERY unknown act degrades to the neutral spec rather
// than crashing or silently wearing another act's colour.

export const ACT_CHIPS: Record<string, ChipSpec> = {
  issue: { token: "--red", icon: "Warn" },
  question: { token: "--blue", icon: "Comment" },
  suggestion: { token: "--accent-soft", icon: "Pen" },
  nitpick: { token: "--ink-dim", icon: "Dot" },
  praise: { token: "--green", icon: "Spark" },
  note: { token: "--ink-mute", icon: "Note" },
  todo: { token: "--warn", icon: "Tasks" },
  chore: { token: "--ink-mute", icon: "Refresh" },
};

/// The neutral fallback for an act this build does not know.
export const ACT_CHIP_UNKNOWN: ChipSpec = { token: "--ink-mute", icon: "Dot" };

export function actChip(act: string | undefined): ChipSpec {
  return ACT_CHIPS[(act ?? "issue").toLowerCase()] ?? ACT_CHIP_UNKNOWN;
}

// ── category chips ────────────────────────────────────────────────────────
// `category` is free-form agent prose ("Correctness", "Style", …) — there
// is NO closed server vocabulary to mirror. The known rows below cover the
// categories the review prompts actually emit; anything else gets a
// DETERMINISTIC token from a small safe list (FNV-1a over the name, the
// `lib/provHue.ts` precedent — identity colouring, never a verdict) so the
// same category always lands on the same chip.

export const CATEGORY_CHIPS: Record<string, ChipSpec> = {
  correctness: { token: "--red", icon: "Warn" },
  security: { token: "--red", icon: "Unlink" },
  performance: { token: "--warn", icon: "Flame" },
  style: { token: "--blue", icon: "Palette" },
  docs: { token: "--green", icon: "Note" },
  documentation: { token: "--green", icon: "Note" },
  tests: { token: "--blue", icon: "Check" },
  test: { token: "--blue", icon: "Check" },
  maintainability: { token: "--accent-soft", icon: "Layers" },
  readability: { token: "--accent-soft", icon: "List" },
};

/// The fallback palette for an unlisted category — deliberate, muted,
/// verdict-neutral tokens only (no `--red`: a category is not a severity).
export const CATEGORY_FALLBACK_TOKENS: readonly string[] = [
  "--blue",
  "--warn",
  "--green",
  "--accent-soft",
  "--ink-mute",
];

/// FNV-1a 32-bit — the same hash `lib/provHue.ts` uses for session hues.
/// Deterministic across runs and engines (no `Math.random`, no locale).
export function fnv1a(input: string): number {
  let h = 0x811c9dc5;
  for (let i = 0; i < input.length; i += 1) {
    h ^= input.charCodeAt(i);
    h = Math.imul(h, 0x01000193);
  }
  return h >>> 0;
}

export function categoryChip(category: string): ChipSpec {
  const known = CATEGORY_CHIPS[category.toLowerCase()];
  if (known) return known;
  const token = CATEGORY_FALLBACK_TOKENS[fnv1a(category) % CATEGORY_FALLBACK_TOKENS.length];
  return { token, icon: "Bookmark" };
}

// ── section decorators ────────────────────────────────────────────────────
// One icon + ONE colour token per section kind, rendered by
// `RoomChips.tsx`'s `SectionDecor`. The kinds are the Room's own section
// vocabulary — `ReportPanel`'s sections plus the Timeline tab's decorator.

export type RoomSectionKind = "summary" | "findings" | "praise" | "questions" | "verdict" | "timeline";

export const SECTION_DECORS: Record<RoomSectionKind, ChipSpec & { label: string }> = {
  summary: { token: "--accent-soft", icon: "Note", label: "Summary" },
  findings: { token: "--warn", icon: "Warn", label: "Findings" },
  praise: { token: "--green", icon: "Spark", label: "Praise" },
  questions: { token: "--blue", icon: "Comment", label: "Questions" },
  verdict: { token: "--accent", icon: "ClipboardCheck", label: "Verdict" },
  timeline: { token: "--ink-mute", icon: "Clock", label: "Timeline" },
};

export function sectionDecor(kind: RoomSectionKind): ChipSpec & { label: string } {
  return SECTION_DECORS[kind];
}

// ── hero derivations ──────────────────────────────────────────────────────
// Every number the Report hero prints comes off the wire through these —
// `liveFindingCounts` (this module's ONE derivation, re-exported by
// `ReportPanel`) for the three counts, `filesViewedOf` for the viewed
// meter. A hero that computed its own count would be the "three
// disagreeing usages numbers" failure web-code/CLAUDE.md names.

export function heroCounts(findings: ReviewFinding[]): LiveCounts {
  return liveFindingCounts(findings);
}

/// "N of M files viewed" — K2a's viewed state, the same predicate
/// `ReviewHeader`'s progress bar already uses (`viewed && !viewed_stale`:
/// a stale viewed mark is not a viewed file).
export function filesViewedOf(files: ReviewFileRow[]): { viewed: number; total: number } {
  return { viewed: files.filter((f) => f.viewed && !f.viewed_stale).length, total: files.length };
}

export interface HeroAgent {
  /// The report's `authored_by` (the model/agent string), when authored.
  author?: string;
  /// The report's `session_id`, falling back to the review's own binding.
  sessionId?: string;
}

/// The hero's agent block — `null` (HIDDEN, never an "UNSET" box) when
/// neither the report nor the review names an agent or a session.
export function heroAgentOf(
  report: Pick<ReviewReport, "authored_by" | "session_id">,
  reviewSessionId: string | null,
): HeroAgent | null {
  const author = report.authored_by?.trim() || undefined;
  const sessionId = report.session_id?.trim() || reviewSessionId || undefined;
  if (!author && !sessionId) return null;
  return { author, sessionId };
}

/// The hero lede: the report's one-line `deck` when authored, else the
/// summary's FIRST paragraph (never the whole body — Section 01 renders
/// that). `null` when the report carries neither.
export function heroLedeOf(report: Pick<ReviewReport, "deck" | "summary">): string | null {
  const deck = report.deck?.trim();
  if (deck) return deck;
  const first = report.summary?.split(/\n\s*\n/)[0]?.trim();
  return first || null;
}

/// `base_source` rides NO review wire today (`ReviewDetail` has no such
/// field; the shape exists on `branches::BranchReviewOut` only). The hero
/// reads it as an OPTIONAL structural field — rendered when a future wire
/// carries it, absent (never "unknown") when it does not.
export function heroBaseSourceOf(review: object): string | null {
  const v = (review as { base_source?: unknown }).base_source;
  return typeof v === "string" && v.trim() ? v : null;
}

// ── truncation ────────────────────────────────────────────────────────────

/// Middle-truncation for long identifiers/paths: keeps BOTH ends (the
/// discriminating parts of `very/long/path/…/file.ts` and of a sha), joins
/// on a single ellipsis, and returns the input untouched when it fits. The
/// full value always rides the element's `title` at the call site — this
/// helper is display-only and never feeds a URL or a lookup.
export function truncateMiddle(value: string, max = 48): string {
  if (value.length <= max) return value;
  if (max < 8) return value.slice(0, max);
  const keep = max - 1; // one ellipsis char
  const head = Math.ceil(keep / 2);
  const tail = Math.floor(keep / 2);
  return `${value.slice(0, head)}…${value.slice(value.length - tail)}`;
}

// ── density ───────────────────────────────────────────────────────────────
// The Room's compact/comfortable toggle. The PREFERENCE's home is
// localStorage (D16's browser-local ruling — same as the global
// `data-density` contract); `?density=` only MIRRORS it so a shared link
// carries it (`lib/branchViews.ts`'s `?density=` precedent, K2a).

export type RoomDensity = "comfortable" | "compact";

export const ROOM_DENSITY_STORAGE_KEY = "kbc:review-room-density";

/// TOTAL parse: anything that is not literally `compact` reads as the
/// default — a stale bookmark must never blank the page.
export function parseRoomDensity(v: string | null): RoomDensity {
  return v === "compact" ? "compact" : "comfortable";
}

export function nextRoomDensity(d: RoomDensity): RoomDensity {
  return d === "compact" ? "comfortable" : "compact";
}
