import { describe, expect, it } from "vitest";
import {
  attentionTintStyle,
  clampScore,
  formatAttentionScore,
  notAvailableLabel,
} from "./attentionRamp";

describe("clampScore", () => {
  it("clamps to 0..1", () => {
    expect(clampScore(-1)).toBe(0);
    expect(clampScore(0)).toBe(0);
    expect(clampScore(0.5)).toBe(0.5);
    expect(clampScore(1)).toBe(1);
    expect(clampScore(2)).toBe(1);
  });

  it("treats non-finite as 0", () => {
    expect(clampScore(Number.NaN)).toBe(0);
    expect(clampScore(Number.POSITIVE_INFINITY)).toBe(0);
  });
});

describe("attentionTintStyle", () => {
  it("returns undefined for null/undefined (absence ≠ zero)", () => {
    expect(attentionTintStyle(null)).toBeUndefined();
    expect(attentionTintStyle(undefined)).toBeUndefined();
  });

  it("uses accent color-mix, never red", () => {
    const s = attentionTintStyle(1)!;
    expect(s.backgroundColor).toContain("var(--accent)");
    expect(s.backgroundColor.toLowerCase()).not.toContain("red");
    expect(s.backgroundColor).not.toContain("var(--red)");
  });

  it("higher score → higher opacity percent", () => {
    const low = attentionTintStyle(0.1)!;
    const high = attentionTintStyle(0.9)!;
    const lowPct = Number(low.backgroundColor.match(/(\d+)%/)?.[1] ?? 0);
    const highPct = Number(high.backgroundColor.match(/(\d+)%/)?.[1] ?? 0);
    expect(highPct).toBeGreaterThan(lowPct);
  });
});

describe("formatAttentionScore", () => {
  it("renders em dash for null (never a zero badge)", () => {
    expect(formatAttentionScore(null)).toBe("—");
    expect(formatAttentionScore(undefined)).toBe("—");
  });

  it("formats finite scores", () => {
    expect(formatAttentionScore(0.5)).toBe("0.50");
    expect(formatAttentionScore(1, 1)).toBe("1.0");
  });
});

describe("notAvailableLabel", () => {
  it("names the missing term", () => {
    expect(notAvailableLabel("session_pain")).toBe("not available: session_pain");
  });
});
