// T18 — "Fit to phone" arithmetic.

import { describe, expect, it } from "vitest";
import { fitDims } from "./fitPhone";

describe("fitDims", () => {
  it("floors width over the cell width and height over the cell height", () => {
    expect(fitDims({ visibleWidth: 390, visibleHeight: 400, cellWidth: 7.2, cellHeight: 17 })).toEqual({
      cols: 54,
      rows: 23,
    });
    // Exactly divisible: no off-by-one.
    expect(fitDims({ visibleWidth: 90, visibleHeight: 36, cellWidth: 9, cellHeight: 18 })).toEqual({
      cols: 10,
      rows: 2,
    });
  });

  it("is null when anything is unmeasurable", () => {
    const ok = { visibleWidth: 390, visibleHeight: 400, cellWidth: 7, cellHeight: 17 };
    expect(fitDims(null)).toBeNull();
    for (const bad of [
      { ...ok, visibleWidth: 0 },
      { ...ok, visibleHeight: -1 },
      { ...ok, cellWidth: 0 },
      { ...ok, cellHeight: Number.NaN },
      { ...ok, visibleWidth: Number.POSITIVE_INFINITY },
      { ...ok, visibleWidth: 3 },
    ]) {
      expect(fitDims(bad)).toBeNull();
    }
  });
});
