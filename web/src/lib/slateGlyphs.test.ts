import { describe, expect, it } from "vitest";
import {
  AGE_OPACITY_FLOOR,
  KIND_GLYPHS,
  STATE_GLYPHS,
  authorChip,
  colorForKind,
  glyphForKind,
  glyphForLiveness,
  opacityForAge,
  type SlateColorToken,
} from "./slateGlyphs";
import type { SlateKind, SlateLiveness } from "../api/slateTypes";

// The design's non-negotiable: "Every glyph has a text alternative (the kind
// word), so nothing is conveyed by emoji or colour alone." These cases are
// that sentence, executable.

const ALL_KINDS: SlateKind[] = [
  "now",
  "warn",
  "take",
  "done",
  "hand",
  "ask",
  "answer",
  "found",
  "idea",
  "tried",
  "drop",
  "mark",
];

const FAMILIES: Record<string, SlateColorToken> = {
  now: "--accent",
  warn: "--accent",
  take: "--blue",
  done: "--blue",
  hand: "--blue",
  ask: "--warn",
  answer: "--warn",
  found: "--ok",
  idea: "--ok",
  tried: "--danger",
  drop: "--muted",
  mark: "--muted",
};

describe("slateGlyphs — every kind and state has a glyph AND a word", () => {
  it("covers all twelve kinds with a non-empty glyph and word", () => {
    expect(Object.keys(KIND_GLYPHS).sort()).toEqual([...ALL_KINDS].sort());
    for (const k of ALL_KINDS) {
      const g = glyphForKind(k);
      expect(g.glyph.length, `${k} glyph`).toBeGreaterThan(0);
      expect(g.word.length, `${k} word`).toBeGreaterThan(0);
      // The word is the digest's own capitalised kind word (Kind::word()),
      // so board and CLI name a post the same thing.
      expect(g.word).toBe(k.toUpperCase());
    }
  });

  it("assigns every kind the design's colour family", () => {
    for (const k of ALL_KINDS) {
      expect(glyphForKind(k).token, `${k} family`).toBe(FAMILIES[k]);
      expect(colorForKind(k)).toBe(`var(${FAMILIES[k]})`);
    }
  });

  it("colours are tokens.css custom-property NAMES, never literals", () => {
    for (const g of [
      ...Object.values(KIND_GLYPHS),
      ...Object.values(STATE_GLYPHS),
    ]) {
      expect(g.token).toMatch(/^--[a-z]+$/);
    }
  });

  it("covers every state badge with a glyph and a word", () => {
    const states = ["pin", "mark", "contested", "live", "stale", "expired", "human"];
    expect(Object.keys(STATE_GLYPHS).sort()).toEqual([...states].sort());
    for (const s of states) {
      const g = STATE_GLYPHS[s as keyof typeof STATE_GLYPHS];
      expect(g.glyph.length).toBeGreaterThan(0);
      expect(g.word.length).toBeGreaterThan(0);
    }
  });

  it("maps each liveness value to its own badge, with `stale?` honest about being derived", () => {
    const l: SlateLiveness[] = ["live", "stale", "expired"];
    const words = l.map((v) => glyphForLiveness(v).word);
    expect(words).toEqual(["live", "stale?", "expired"]);
    expect(new Set(l.map((v) => glyphForLiveness(v).glyph)).size).toBe(3);
  });

  it("an unknown wire kind degrades to a dot plus the raw word, never a blank card", () => {
    const g = glyphForKind("sketch" as SlateKind);
    expect(g.glyph).toBe("•");
    expect(g.word).toBe("SKETCH");
  });
});

describe("slateGlyphs — age fade", () => {
  it("buckets exactly as §10 pins: <1h 1, <8h .85, <2d .7, older .55", () => {
    expect(opacityForAge(0)).toBe(1);
    expect(opacityForAge(3599)).toBe(1);
    expect(opacityForAge(3600)).toBe(0.85);
    expect(opacityForAge(8 * 3600 - 1)).toBe(0.85);
    expect(opacityForAge(8 * 3600)).toBe(0.7);
    expect(opacityForAge(2 * 86400 - 1)).toBe(0.7);
    expect(opacityForAge(2 * 86400)).toBe(0.55);
    expect(opacityForAge(400 * 86400)).toBe(0.55);
  });

  it("never falls below the AA floor, however old", () => {
    for (const s of [0, 1, 3600, 86400, 1e9, Number.MAX_SAFE_INTEGER]) {
      expect(opacityForAge(s)).toBeGreaterThanOrEqual(AGE_OPACITY_FLOOR);
    }
  });

  it("a nonsense age reads as fresh rather than as invisible", () => {
    expect(opacityForAge(Number.NaN)).toBe(1);
    expect(opacityForAge(-5)).toBe(1);
  });
});

describe("slateGlyphs — the author chip", () => {
  it("prefers the daemon's own composed tag", () => {
    expect(
      authorChip({ origin: "agent", harness: "codex", session_short: "8f2a", tag: "codex/8f2a" }),
    ).toBe("codex/8f2a");
  });
  it("falls back per origin when a row predates `tag`", () => {
    expect(authorChip({ origin: "human", harness: "spa", session_short: "" })).toBe("you");
    expect(
      authorChip({ origin: "import", harness: "job", session_short: "", job_id: "01M11" }),
    ).toBe("job:01M11");
    expect(
      authorChip({ origin: "agent", harness: "claude", session_short: "4b7e" }),
    ).toBe("claude/4b7e");
    expect(authorChip({ origin: "agent", harness: "kimi", session_short: "" })).toBe("kimi");
  });
});
