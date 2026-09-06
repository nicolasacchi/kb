import { describe, expect, it } from "vitest";
import { impactChipText, topChangedSymbol } from "./reviewImpact";
import type { ReviewImpactFileOut } from "../api/types";

function out(overrides: Partial<ReviewImpactFileOut> = {}): ReviewImpactFileOut {
  return {
    schema: "review-impact-file/1",
    review_id: 1,
    repo: "r",
    ps_number: 1,
    path: "a.ts",
    lang_supported: true,
    changed_symbols: [],
    callers_total: 0,
    callers_in_diff: 0,
    callers_out_of_diff: 0,
    symbols_truncated: false,
    note: "note",
    ...overrides,
  };
}

describe("impactChipText", () => {
  it("is null when absent", () => {
    expect(impactChipText(undefined)).toBeNull();
    expect(impactChipText(null)).toBeNull();
  });
  it("is null for an unsupported language", () => {
    expect(impactChipText(out({ lang_supported: false, callers_total: 5 }))).toBeNull();
  });
  it("is null when there are zero callers (never a fabricated zero chip)", () => {
    expect(impactChipText(out({ callers_total: 0 }))).toBeNull();
  });
  it("singularizes 1 caller", () => {
    expect(impactChipText(out({ callers_total: 1, callers_in_diff: 1 }))).toBe("1 caller · 1 in this diff");
  });
  it("pluralizes N callers", () => {
    expect(impactChipText(out({ callers_total: 12, callers_in_diff: 2 }))).toBe(
      "12 callers · 2 in this diff",
    );
  });
});

describe("topChangedSymbol", () => {
  it("is null with no changed symbols", () => {
    expect(topChangedSymbol(out())).toBeNull();
    expect(topChangedSymbol(undefined)).toBeNull();
  });
  it("picks the symbol with the most total callers", () => {
    const data = out({
      changed_symbols: [
        { name: "a", kind: "function", line: 10, col: 0, callers_total: 2, callers_in_diff: 1, callers_out_of_diff: 1, truncated: false },
        { name: "b", kind: "function", line: 20, col: 0, callers_total: 9, callers_in_diff: 3, callers_out_of_diff: 6, truncated: false },
      ],
    });
    expect(topChangedSymbol(data)?.name).toBe("b");
  });
  it("breaks a tie by line ascending", () => {
    const data = out({
      changed_symbols: [
        { name: "later", kind: "function", line: 40, col: 0, callers_total: 3, callers_in_diff: 0, callers_out_of_diff: 3, truncated: false },
        { name: "earlier", kind: "function", line: 5, col: 0, callers_total: 3, callers_in_diff: 0, callers_out_of_diff: 3, truncated: false },
      ],
    });
    expect(topChangedSymbol(data)?.name).toBe("earlier");
  });
});
