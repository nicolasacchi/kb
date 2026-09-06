// W3.B — THE chip vocabulary for session surfaces (synthesis-memo R11: "Chip
// vocabulary: surfaces' `sessionChips.ts` is the ONE home; projects P9's
// glyph column dropped"). Pure functions over already-fetched `SessionRow`
// fields — no fetch, no state, no JSX. Consumed by the sessions list (S1),
// the gallery/search session cards (S7), and the reader's adaptive rail
// (S4) so every surface renders the same glyphs for the same facts.

import type { SessionRow } from "../api/sessions";

// ---- harness -----------------------------------------------------------

/// The closed harness set (`kb_core::sessions::HARNESSES`) — glyphs match
/// designs/projects.md P9's proposal (the glyph COLUMN there was dropped in
/// favour of this module owning the vocabulary; the glyphs themselves carry
/// over since nothing superseded them).
export const HARNESS_GLYPH: Record<string, string> = {
  claude: "◆",
  codex: "⬢",
  opencode: "⬡",
  grok: "✦",
  kimi: "☾",
  // OK1 — Oh My Pi.
  omp: "π",
};

/// Glyph for a harness value. Unknown/absent harnesses (forward-compat with
/// a harness added after this ships) fall back to a neutral dot rather than
/// guessing — `harness` is `NOT NULL DEFAULT 'claude'` server-side, so
/// "absent" only happens against a stale/mocked payload.
export function harnessGlyph(harness: string | null | undefined): string {
  if (!harness) return HARNESS_GLYPH.claude;
  return HARNESS_GLYPH[harness] ?? "●";
}

// ---- badges (S1: "✓N commits, ✗ errors, ◇N memories") ------------------

export function commitBadge(commitCount: number): string | null {
  return commitCount > 0 ? `✓${commitCount}` : null;
}

export function errorBadge(errorCount: number): string | null {
  return errorCount > 0 ? "✗" : null;
}

export function memoryBadge(memoryCount: number): string | null {
  return memoryCount > 0 ? `◇${memoryCount}` : null;
}

export function subagentBadge(subagentCount: number): string | null {
  return subagentCount > 0 ? "⚑" : null;
}

/// W6 (moonshots M4) — the project ledger's decisions-per-day count. Distinct
/// glyph from every other badge here (✓ commits, ✗ errors, ◇ memories, ⚑
/// subagents, ∅ husk).
export function decisionBadge(decisionCount: number): string | null {
  return decisionCount > 0 ? `⌥${decisionCount}` : null;
}

// ---- substance / husk triage (S1/S7/R4) ---------------------------------

export type Substance = "trivial" | "routine" | "substantive";

/// R4 — `NULL` substance (an un-backfilled row) is ALWAYS treated as
/// `"substantive"`: un-backfilled history is never hidden or dimmed by a
/// husk filter/badge.
export function normalizeSubstance(
  substance: string | null | undefined,
): Substance {
  return substance === "trivial" || substance === "routine"
    ? substance
    : "substantive";
}

export function isHusk(substance: string | null | undefined): boolean {
  return normalizeSubstance(substance) === "trivial";
}

/// S7 — the `∅` badge on a dimmed husk card/row. `null` for anything that
/// isn't a trivial capture (routine/substantive/unbackfilled never badge).
export function substanceBadge(substance: string | null | undefined): string | null {
  return isHusk(substance) ? "∅" : null;
}

// ---- outcome presence (S1: "» outcome_preview, first_user_prompt fallback") --

export type OutcomeLine = {
  text: string;
  /// `true` when `text` is the session's CLOSURE (`outcome`); `false` when
  /// it fell back to `first_user_prompt` (a pre-backfill row, or a session
  /// with no closing prose). The `»` prefix in S1's row anatomy is drawn
  /// only when this is `true`, so degrade-to-opening-prompt is visible, not
  /// silent.
  isOutcome: boolean;
};

/// The row-2 preview line: the session's outcome when present, else its
/// opening prompt. `null` when the session has neither (an un-parsed row).
export function outcomeLine(
  outcome: string | null | undefined,
  firstUserPrompt: string | null | undefined,
): OutcomeLine | null {
  if (outcome && outcome.trim()) return { text: outcome, isOutcome: true };
  if (firstUserPrompt && firstUserPrompt.trim()) {
    return { text: firstUserPrompt, isOutcome: false };
  }
  return null;
}

/// Convenience overload reading straight off a `SessionRow`/`RecollectSessionOut`
/// -shaped object (anything carrying `outcome` + `first_user_prompt`).
export function outcomeLineFor(row: {
  outcome?: string | null;
  first_user_prompt?: string | null;
}): OutcomeLine | null {
  return outcomeLine(row.outcome, row.first_user_prompt);
}

// ---- duration (S1/R6/D4: honest active time, not wall-clock) ------------

/// Format `active_secs` (the per-delta-clamped honest active-work duration,
/// `kb_core::sessions::ACTIVE_DELTA_CLAMP_SECS`). Grammar is seconds-floor
/// (`2h 0m`, `0s`) — slightly denser than wall-clock `humanizeDuration`
/// (`2h`, `""` for sub-second) so active-time chips never go blank.
export function formatActiveDuration(activeSecs: number): string {
  const s = Math.max(0, Math.floor(activeSecs));
  if (s < 60) return `${s}s`;
  const m = Math.floor(s / 60);
  if (m < 60) return `${m}m`;
  const h = Math.floor(m / 60);
  return `${h}h ${m % 60}m`;
}

// ---- the full row chip set (S1's "glyph cluster") ------------------------

export type SessionChips = {
  harness: string;
  commit: string | null;
  error: string | null;
  memory: string | null;
  subagent: string | null;
  substance: string | null;
  outcome: OutcomeLine | null;
  activeDuration: string;
};

/// Compute every chip a session row needs in one call — the shape S1's row,
/// S4's rail badges, and S7's gallery card all read from. Pure; takes only
/// the fields it needs so callers can pass a `SessionRow`, a
/// `RecollectSessionOut`, or a hand-built fixture in tests.
export function sessionChips(row: {
  harness?: string | null;
  commit_count?: number | null;
  error_count?: number | null;
  memory_count?: number | null;
  subagent_count?: number | null;
  substance?: string | null;
  outcome?: string | null;
  first_user_prompt?: string | null;
  active_secs?: number | null;
}): SessionChips {
  return {
    harness: harnessGlyph(row.harness),
    commit: commitBadge(row.commit_count ?? 0),
    error: errorBadge(row.error_count ?? 0),
    memory: memoryBadge(row.memory_count ?? 0),
    subagent: subagentBadge(row.subagent_count ?? 0),
    substance: substanceBadge(row.substance),
    outcome: outcomeLine(row.outcome, row.first_user_prompt),
    activeDuration: formatActiveDuration(row.active_secs ?? 0),
  };
}

/// Typed convenience wrapper over a real `SessionRow`.
export function sessionRowChips(row: SessionRow): SessionChips {
  return sessionChips({
    harness: row.harness,
    commit_count: row.commit_count,
    error_count: row.error_count,
    memory_count: row.memory_count,
    subagent_count: row.subagent_count,
    substance: row.substance,
    outcome: row.outcome,
    first_user_prompt: row.first_user_prompt,
    active_secs: row.active_secs,
  });
}
