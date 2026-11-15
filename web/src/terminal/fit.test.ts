import { describe, expect, it } from "vitest";
import { computeFontSize } from "./fit";

describe("computeFontSize", () => {
  it("clamps between 10 and 16", () => {
    // 1000px / (120 cols * 0.6 ratio) = 13.8 -> 13
    expect(computeFontSize(1000, 120, 0.6)).toBe(13);
    // Very wide container clamps to 16.
    expect(computeFontSize(100000, 120, 0.6)).toBe(16);
    // Very narrow clamps to the 10 floor.
    expect(computeFontSize(100, 120, 0.6)).toBe(10);
  });

  it("returns the floor for invalid geometry", () => {
    expect(computeFontSize(0, 120, 0.6)).toBe(10);
    expect(computeFontSize(1000, 0, 0.6)).toBe(10);
    expect(computeFontSize(1000, 120, 0)).toBe(10);
  });
});
