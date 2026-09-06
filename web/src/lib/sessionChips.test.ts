import { describe, expect, it } from "vitest";
import {
  commitBadge,
  decisionBadge,
  errorBadge,
  formatActiveDuration,
  harnessGlyph,
  isHusk,
  memoryBadge,
  normalizeSubstance,
  outcomeLine,
  outcomeLineFor,
  sessionChips,
  subagentBadge,
  substanceBadge,
} from "./sessionChips";

describe("harnessGlyph", () => {
  it("maps the closed harness set", () => {
    expect(harnessGlyph("claude")).toBe("◆");
    expect(harnessGlyph("codex")).toBe("⬢");
    expect(harnessGlyph("opencode")).toBe("⬡");
    expect(harnessGlyph("grok")).toBe("✦");
    expect(harnessGlyph("kimi")).toBe("☾");
    expect(harnessGlyph("omp")).toBe("π");
  });

  it("defaults absent to claude, and an unknown value to a neutral dot", () => {
    expect(harnessGlyph(null)).toBe("◆");
    expect(harnessGlyph(undefined)).toBe("◆");
    expect(harnessGlyph("some-future-harness")).toBe("●");
  });
});

describe("badges", () => {
  it("commitBadge shows a check-count only when > 0", () => {
    expect(commitBadge(0)).toBeNull();
    expect(commitBadge(3)).toBe("✓3");
  });

  it("errorBadge is a bare glyph only when > 0", () => {
    expect(errorBadge(0)).toBeNull();
    expect(errorBadge(1)).toBe("✗");
  });

  it("memoryBadge shows a count only when > 0", () => {
    expect(memoryBadge(0)).toBeNull();
    expect(memoryBadge(2)).toBe("◇2");
  });

  it("subagentBadge is a bare glyph only when > 0", () => {
    expect(subagentBadge(0)).toBeNull();
    expect(subagentBadge(1)).toBe("⚑");
  });

  it("decisionBadge (W6 — the project ledger) shows a count only when > 0", () => {
    expect(decisionBadge(0)).toBeNull();
    expect(decisionBadge(2)).toBe("⌥2");
  });
});

describe("substance", () => {
  it("normalizeSubstance treats null/undefined/unknown as substantive (R4)", () => {
    expect(normalizeSubstance(null)).toBe("substantive");
    expect(normalizeSubstance(undefined)).toBe("substantive");
    expect(normalizeSubstance("substantive")).toBe("substantive");
    expect(normalizeSubstance("routine")).toBe("routine");
    expect(normalizeSubstance("trivial")).toBe("trivial");
  });

  it("isHusk / substanceBadge fire only on trivial", () => {
    expect(isHusk("trivial")).toBe(true);
    expect(isHusk("routine")).toBe(false);
    expect(isHusk(null)).toBe(false);
    expect(substanceBadge("trivial")).toBe("∅");
    expect(substanceBadge("routine")).toBeNull();
    expect(substanceBadge(null)).toBeNull();
  });
});

describe("outcomeLine", () => {
  it("prefers the outcome, flagged isOutcome=true", () => {
    expect(outcomeLine("shipped the fix", "do the thing")).toEqual({
      text: "shipped the fix",
      isOutcome: true,
    });
  });

  it("falls back to first_user_prompt, flagged isOutcome=false", () => {
    expect(outcomeLine(null, "do the thing")).toEqual({
      text: "do the thing",
      isOutcome: false,
    });
    expect(outcomeLine("   ", "do the thing")).toEqual({
      text: "do the thing",
      isOutcome: false,
    });
  });

  it("is null when neither is present", () => {
    expect(outcomeLine(null, null)).toBeNull();
    expect(outcomeLine("  ", "  ")).toBeNull();
  });

  it("outcomeLineFor reads the same pair off an object", () => {
    expect(
      outcomeLineFor({ outcome: "done", first_user_prompt: "start" }),
    ).toEqual({ text: "done", isOutcome: true });
  });
});

describe("formatActiveDuration", () => {
  it("formats seconds/minutes/hours like the wall-clock formatter", () => {
    expect(formatActiveDuration(45)).toBe("45s");
    expect(formatActiveDuration(90)).toBe("1m");
    expect(formatActiveDuration(3661)).toBe("1h 1m");
  });

  it("clamps a negative/garbage input to 0s rather than throwing", () => {
    expect(formatActiveDuration(-5)).toBe("0s");
  });
});

describe("sessionChips", () => {
  it("computes the full chip set from a plain object", () => {
    const chips = sessionChips({
      harness: "codex",
      commit_count: 2,
      error_count: 1,
      memory_count: 3,
      subagent_count: 1,
      substance: "substantive",
      outcome: "shipped it",
      first_user_prompt: "do the thing",
      active_secs: 125,
    });
    expect(chips).toEqual({
      harness: "⬢",
      commit: "✓2",
      error: "✗",
      memory: "◇3",
      subagent: "⚑",
      substance: null,
      outcome: { text: "shipped it", isOutcome: true },
      activeDuration: "2m",
    });
  });

  it("defaults missing counts to 0/absent gracefully", () => {
    const chips = sessionChips({});
    expect(chips.commit).toBeNull();
    expect(chips.error).toBeNull();
    expect(chips.memory).toBeNull();
    expect(chips.subagent).toBeNull();
    expect(chips.outcome).toBeNull();
    expect(chips.activeDuration).toBe("0s");
    expect(chips.harness).toBe("◆");
  });
});
