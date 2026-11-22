import { describe, expect, it } from "vitest";
import { bellNeedsAttention, PROG_TITLE_MAX, sanitizeOscTitle, titleWithBadge } from "./termAttention";

describe("titleWithBadge", () => {
  it("prefixes the count and leaves a quiet title alone", () => {
    expect(titleWithBadge("kb", 1)).toBe("(1) kb");
    expect(titleWithBadge("Sessions · kb", 3)).toBe("(3) Sessions · kb");
    expect(titleWithBadge("kb", 0)).toBe("kb");
    expect(titleWithBadge("kb", -2)).toBe("kb");
    expect(titleWithBadge("kb", Number.NaN)).toBe("kb");
  });
});

describe("sanitizeOscTitle", () => {
  it("keeps markup as inert text and drops controls and bidi marks", () => {
    expect(sanitizeOscTitle("<img src=x onerror=1>")).toBe("<img src=x onerror=1>");
    expect(sanitizeOscTitle("a\u001b[31mb\u0007c")).toBe("a [31mb c");
    expect(sanitizeOscTitle("x\u202ey")).toBe("x y");
  });
  it("collapses whitespace, caps with an ellipsis, and tolerates junk", () => {
    expect(sanitizeOscTitle("  a \n\t b  ")).toBe("a b");
    const long = sanitizeOscTitle("w".repeat(500));
    expect(long.length).toBe(PROG_TITLE_MAX);
    expect(long.endsWith("…")).toBe(true);
    expect(sanitizeOscTitle("\u0001\u0002")).toBe("");
    expect(sanitizeOscTitle(undefined)).toBe("");
  });
});

describe("bellNeedsAttention", () => {
  it("is quiet only for a visible, focused terminal", () => {
    expect(bellNeedsAttention({ hidden: false, termFocused: true })).toBe(false);
    expect(bellNeedsAttention({ hidden: true, termFocused: true })).toBe(true);
    expect(bellNeedsAttention({ hidden: false, termFocused: false })).toBe(true);
  });
});
