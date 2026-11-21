import { describe, expect, it } from "vitest";
import { FIT_MAX_INLINE, FIT_MIN_INLINE, computeFit, computeFontSize } from "./fit";

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

// A JetBrains-Mono-like cell, per px of font size.
const CELL = { cellW: 0.6, cellH: 1.32 };
const fit = (width: number, height: number, cols: number, rows: number, over: Partial<Parameters<typeof computeFit>[0]> = {}) =>
  computeFit({ width, height, cols, rows, ...CELL, min: FIT_MIN_INLINE, max: FIT_MAX_INLINE, ...over });

describe("computeFit", () => {
  it("fits a 201x48 grid at 1400x800 above the floor and uncropped", () => {
    const r = fit(1400, 800, 201, 48);
    expect(r.fontSize).toBe(11);
    expect(r.fontSize).toBeGreaterThanOrEqual(9);
    expect(r.cropped).toEqual({ x: false, y: false });
  });

  it("crops on x when the width cannot hold 201 columns at the floor", () => {
    const r = fit(800, 800, 201, 48);
    expect(r.fontSize).toBe(9);
    expect(r.cropped).toEqual({ x: true, y: false });
  });

  it("takes the smaller of the width fit and the height fit", () => {
    // Plenty of width (font 39 by width), but 24 rows in 250px allows 7: the floor, cropped on y.
    expect(fit(1900, 250, 80, 24).fontSize).toBe(9);
    expect(fit(1900, 250, 80, 24).cropped.y).toBe(true);
    // 24 rows in 400px allows 12: the height fit beats the width fit.
    expect(fit(1900, 400, 80, 24).fontSize).toBe(12);
    // Plenty of height, little width: the width fit wins.
    expect(fit(300, 2000, 80, 24).fontSize).toBe(9);
    expect(fit(500, 2000, 80, 24).fontSize).toBe(10);
  });

  it("hits the max for a small grid in a big window", () => {
    const r = fit(1900, 1000, 80, 24);
    expect(r.fontSize).toBe(20);
    expect(r.cropped).toEqual({ x: false, y: false });
  });

  it("honours a caller-supplied floor (full screen passes 8)", () => {
    expect(fit(600, 800, 201, 48, { min: 8 }).fontSize).toBe(8);
  });

  it("zero, negative or NaN geometry gives the floor and no crop, and never throws", () => {
    for (const bad of [0, -5, Number.NaN]) {
      expect(fit(bad, 800, 201, 48)).toEqual({ fontSize: 9, cropped: { x: false, y: false } });
      expect(fit(1400, 800, bad, 48)).toEqual({ fontSize: 9, cropped: { x: false, y: false } });
      expect(fit(1400, 800, 201, 48, { cellW: bad })).toEqual({ fontSize: 9, cropped: { x: false, y: false } });
    }
  });

  it("an unknown height falls back to the width fit alone", () => {
    expect(fit(1400, 0, 201, 48).fontSize).toBe(11);
    expect(fit(1400, Number.NaN, 201, 48).cropped.y).toBe(false);
  });
});
