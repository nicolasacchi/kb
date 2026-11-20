import { describe, expect, it } from "vitest";
import { FIT_REF_FONT, GRID_BOUNDS, fitWindowDims } from "./fitWindow";

const m = (width: number, height: number) => ({ width, height, cellW: 0.6, cellH: 1.3 });

describe("fitWindowDims", () => {
  it("measures the window at the 14px reference font", () => {
    expect(FIT_REF_FONT).toBe(14);
    // 1400 / (0.6 * 14) = 166.6 ; 800 / (1.3 * 14) = 43.9
    expect(fitWindowDims(m(1400, 800))).toEqual({ cols: 166, rows: 43 });
  });

  it("clamps to the daemon bounds on both sides", () => {
    expect(fitWindowDims(m(100, 40))).toEqual({ cols: GRID_BOUNDS.minCols, rows: GRID_BOUNDS.minRows });
    expect(fitWindowDims(m(100000, 100000))).toEqual({ cols: GRID_BOUNDS.maxCols, rows: GRID_BOUNDS.maxRows });
    expect(GRID_BOUNDS).toEqual({ minCols: 20, maxCols: 500, minRows: 5, maxRows: 200 });
  });

  it("is null for an unmeasurable window", () => {
    expect(fitWindowDims(null)).toBeNull();
    expect(fitWindowDims(m(0, 800))).toBeNull();
    expect(fitWindowDims(m(1400, Number.NaN))).toBeNull();
    expect(fitWindowDims({ width: 1400, height: 800, cellW: 0, cellH: 1.3 })).toBeNull();
  });
});
