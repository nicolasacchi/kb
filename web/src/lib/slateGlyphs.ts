// The board's ONE glyph/colour map (design §10 "Glyphs and colour").
//
// PURE — no React, no DOM, no fetch. Two rules the vitest suite pins:
//
//   1. every kind AND every state has a glyph AND a WORD. Nothing on the
//      board is ever conveyed by emoji or colour alone; the card renders
//      the word beside the glyph, and the glyph itself is always
//      `aria-hidden` with the word as its text alternative.
//   2. colour is a `tokens.css` custom property NAME, never a literal —
//      families per the design: status `--accent`, work `--blue`,
//      questions `--warn`, knowledge `--ok`, tried `--danger`,
//      housekeeping `--muted`.
//
// The injected digest gets NONE of this (an emoji costs 2–3 tokens on
// OpenAI encodings and more on Claude's, a kind word costs one, and no
// legend is live for Codex/Kimi/OpenCode/Grok). This file exists so the
// SPA — the one presenter with a screen — can render what the digest
// refuses, off the SAME projection fields.

import type {
  SlateKind,
  SlateLiveness,
  SlateOrigin,
} from "../api/slateTypes";

/// A tokens.css custom-property NAME (used as `var(--blue)`).
export type SlateColorToken =
  | "--accent"
  | "--blue"
  | "--warn"
  | "--ok"
  | "--danger"
  | "--muted";

export type SlateGlyph = {
  /// The emoji. Always rendered `aria-hidden`.
  glyph: string;
  /// The text alternative — ALWAYS rendered beside the glyph.
  word: string;
  /// tokens.css custom-property name; `var(<token>)` at the call site.
  token: SlateColorToken;
};

/// Kind → glyph, word, colour family. The words are `Kind::word()`'s
/// capitals from kb-core, so the board and the digest name a post the
/// same thing.
export const KIND_GLYPHS: Readonly<Record<SlateKind, SlateGlyph>> = {
  // status — --accent
  now: { glyph: "\u{1F9ED}", word: "NOW", token: "--accent" },
  warn: { glyph: "⚠️", word: "WARN", token: "--accent" },
  // work — --blue
  take: { glyph: "✋", word: "TAKE", token: "--blue" },
  done: { glyph: "✅", word: "DONE", token: "--blue" },
  hand: { glyph: "\u{1F91D}", word: "HAND", token: "--blue" },
  // questions — --warn
  ask: { glyph: "❓", word: "ASK", token: "--warn" },
  answer: { glyph: "\u{1F4AC}", word: "ANSWER", token: "--warn" },
  // knowledge — --ok
  found: { glyph: "\u{1F50D}", word: "FOUND", token: "--ok" },
  idea: { glyph: "\u{1F4A1}", word: "IDEA", token: "--ok" },
  // tried — --danger
  tried: { glyph: "⛔", word: "TRIED", token: "--danger" },
  // housekeeping — --muted
  drop: { glyph: "\u{1F5D1}️", word: "DROP", token: "--muted" },
  mark: { glyph: "⭕", word: "MARK", token: "--muted" },
};

/// The STATE glyphs — the badges a card wears beside its kind. Same
/// contract: every one has a word.
export type SlateStateId =
  | "pin"
  | "mark"
  | "contested"
  | "live"
  | "stale"
  | "expired"
  | "human";

export const STATE_GLYPHS: Readonly<Record<SlateStateId, SlateGlyph>> = {
  pin: { glyph: "\u{1F4CC}", word: "pinned", token: "--accent" },
  mark: { glyph: "⭕", word: "marks", token: "--muted" },
  contested: { glyph: "⚡", word: "contested", token: "--danger" },
  live: { glyph: "\u{1F7E2}", word: "live", token: "--ok" },
  stale: { glyph: "\u{1F7E1}", word: "stale?", token: "--warn" },
  expired: { glyph: "⚫", word: "expired", token: "--muted" },
  human: { glyph: "\u{1F464}", word: "you", token: "--accent" },
};

/// Kind lookup. Total by construction (`SlateKind` is a closed union), but
/// the runtime fallback keeps a wire value the SPA has not learned yet from
/// rendering a blank card.
export function glyphForKind(kind: SlateKind): SlateGlyph {
  return (
    KIND_GLYPHS[kind] ?? {
      glyph: "•",
      word: String(kind).toUpperCase(),
      token: "--muted" as const,
    }
  );
}

/// Take liveness → its badge. `stale` renders `stale?` — the question mark
/// is the design's honesty about a derived, unconfirmed state.
export function glyphForLiveness(l: SlateLiveness): SlateGlyph {
  return STATE_GLYPHS[l];
}

/// `var(--token)` for a kind — the ONE place a card's accent is composed.
export function colorForKind(kind: SlateKind): string {
  return `var(${glyphForKind(kind).token})`;
}

/// Age → opacity bucket (§10 "Size and fade"): under 1h full, under 8h
/// .85, under 2d .7, older .55. The floor is .55 and never lower — that is
/// the AA-contrast floor the design pins, so this function is the only
/// place a card's opacity is decided.
export const AGE_OPACITY_FLOOR = 0.55;
export function opacityForAge(ageSecs: number): number {
  if (!Number.isFinite(ageSecs) || ageSecs < 3600) return 1;
  if (ageSecs < 8 * 3600) return 0.85;
  if (ageSecs < 2 * 86400) return 0.7;
  return AGE_OPACITY_FLOOR;
}

/// The author chip's own text. `[you]` for a human post, `job:<ulid>` for
/// an import, `harness/session` for an agent — mirrors `Who::tag`, which
/// the daemon already composed; this is the fallback when a wire row
/// predates it.
export function authorChip(who: {
  origin: SlateOrigin;
  harness: string;
  session_short: string;
  job_id?: string;
  tag?: string;
}): string {
  if (who.tag) return who.tag;
  if (who.origin === "human") return "you";
  if (who.job_id) return `job:${who.job_id}`;
  return who.session_short
    ? `${who.harness}/${who.session_short}`
    : who.harness;
}
