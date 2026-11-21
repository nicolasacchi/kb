// @vitest-environment jsdom
// FUTERMPOLISH W3 — after a window resize and a grid change the font and the
// Cropped hint equal what a fresh load of the same page and grid gives, even
// when the renderer paints a frame AFTER the fit (a measurement straight after
// `resize()` / `fontSize = n` still shows the previous screen).

import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { computeFit, FIT_MAX_INLINE, FIT_MIN_INLINE, type FitResult } from "./fit";
import { fitHostNow } from "./fitHost";
import { createFitter } from "./fitter";

// A 0.6 x 1.32 cell (the probe), painted at whole pixels like a browser.
const CW = 0.6;
const CH = 1.32;
beforeEach(() => {
  vi.spyOn(Element.prototype, "getBoundingClientRect").mockReturnValue({ width: CW * 100 * 20, height: CH * 100 } as DOMRect);
});
afterEach(() => {
  vi.restoreAllMocks();
});

/**
 * A terminal whose screen repaints only on `paint()` (one frame after the change).
 * `lag: "grid"`: the font is painted at once but a resize only on the next frame
 * (an old column count at the new font); `"both"`: nothing is painted before the frame.
 */
function rig(box: { w: number; h: number }, lag: "grid" | "both" = "grid") {
  const term = { cols: 201, rows: 48, options: { fontSize: 11, fontFamily: "monospace" } };
  let painted = { cols: term.cols, rows: term.rows, font: term.options.fontSize };
  const host = document.createElement("div");
  const screen = document.createElement("div");
  screen.className = "xterm-screen";
  host.appendChild(screen);
  Object.defineProperty(host, "clientWidth", { get: () => box.w, configurable: true });
  Object.defineProperty(host, "clientHeight", { get: () => box.h, configurable: true });
  const fontNow = (): number => (lag === "grid" ? term.options.fontSize : painted.font);
  Object.defineProperty(screen, "offsetWidth", { get: () => painted.cols * Math.floor(fontNow() * CW), configurable: true });
  Object.defineProperty(screen, "offsetHeight", { get: () => painted.rows * Math.floor(fontNow() * CH), configurable: true });
  const paint = (): void => {
    painted = { cols: term.cols, rows: term.rows, font: term.options.fontSize };
  };
  return { term, host, paint };
}

function driver(r: ReturnType<typeof rig>, grid: { cols: number; rows: number }, box: { w: number; h: number }) {
  const queue: Array<() => void> = [];
  const results: FitResult[] = [];
  const fitter = createFitter<typeof r.term, HTMLElement>({
    run: (t, h, fresh) => fitHostNow(t, h, grid, { min: FIT_MIN_INLINE, max: FIT_MAX_INLINE, fresh }),
    key: () => `${grid.cols}x${grid.rows}|${box.w}x${box.h}`,
    schedule: (fn) => queue.push(fn),
    cancel: () => {
      queue.length = 0;
    },
    onResult: (res) => results.push(res),
  });
  /** One frame: the renderer paints, then the scheduled verification runs. */
  const frame = (): void => {
    r.paint();
    const fn = queue.shift();
    fn?.();
  };
  const settle = (): void => {
    for (let i = 0; i < 8; i += 1) frame();
  };
  return { fitter, settle, results, queue };
}

/** What a fresh page load gives: the fit planned from nothing, verified on a painted screen. */
function freshFit(grid: { cols: number; rows: number }, box: { w: number; h: number }, lag: "grid" | "both" = "grid"): FitResult {
  const r = rig(box, lag);
  r.term.cols = grid.cols;
  r.term.rows = grid.rows;
  r.paint();
  r.term.options.fontSize = 14;
  const d = driver(r, grid, box);
  d.fitter.refit(r.term, r.host);
  d.settle();
  return d.results[d.results.length - 1];
}

describe.each(["grid", "both"] as const)("fitter: the sequence from the field report (renderer lag: %s)", (lag) => {
  it("box shrink then grid changes end on the fresh-load result (font, Cropped)", () => {
    const box = { w: 1380, h: 700 };
    const grid = { cols: 201, rows: 48 };
    const r = rig(box, lag);
    const d = driver(r, grid, box);
    d.fitter.refit(r.term, r.host);
    d.settle();
    // The window shrinks: same grid, a smaller box.
    box.w = 1230;
    box.h = 576;
    d.fitter.refit(r.term, r.host);
    d.settle();
    for (const g of [
      { cols: 80, rows: 24 },
      { cols: 120, rows: 40 },
      { cols: 201, rows: 48 },
    ]) {
      grid.cols = g.cols;
      grid.rows = g.rows;
      r.term.cols = g.cols;
      r.term.rows = g.rows;
      // The fit runs right after resize(): the screen is still the old one.
      d.fitter.refit(r.term, r.host);
      d.settle();
      const want = freshFit(g, box, lag);
      const last = d.results[d.results.length - 1];
      expect({ grid: g, font: last.fontSize, cropped: last.cropped }).toEqual({ grid: g, font: want.fontSize, cropped: want.cropped });
      expect(r.term.options.fontSize).toBe(want.fontSize);
    }
  });

  it("80x24 after a cropped 201x48 grows to the planned font and drops Cropped (the 14 + Cropped defect)", () => {
    const box = { w: 1230, h: 576 };
    const grid = { cols: 201, rows: 48 };
    const r = rig(box, lag);
    r.term.options.fontSize = 9;
    r.paint();
    const d = driver(r, grid, box);
    d.fitter.refit(r.term, r.host);
    d.settle();
    grid.cols = 80;
    grid.rows = 24;
    r.term.cols = 80;
    r.term.rows = 24;
    d.fitter.refit(r.term, r.host);
    d.settle();
    const want = computeFit({ width: 1230, height: 576, cols: 80, rows: 24, cellW: CW, cellH: CH, min: FIT_MIN_INLINE, max: FIT_MAX_INLINE });
    expect(r.term.options.fontSize).toBe(want.fontSize);
    expect(d.results[d.results.length - 1].cropped).toEqual({ x: false, y: false });
  });

  it("Cropped is true exactly when the floor font cannot hold the grid", () => {
    const box = { w: 800, h: 576 };
    const grid = { cols: 201, rows: 48 };
    const r = rig(box, lag);
    const d = driver(r, grid, box);
    d.fitter.refit(r.term, r.host);
    d.settle();
    expect(r.term.options.fontSize).toBe(FIT_MIN_INLINE);
    expect(d.results[d.results.length - 1].cropped.x).toBe(true);
    grid.cols = 100;
    grid.rows = 24;
    r.term.cols = 100;
    r.term.rows = 24;
    d.fitter.refit(r.term, r.host);
    d.settle();
    expect(d.results[d.results.length - 1].cropped).toEqual({ x: false, y: false });
  });
});

describe("fitter: no ratchet, no stale passes", () => {
  it("repeated refits of the same grid in the same box never shrink the font", () => {
    const box = { w: 1230, h: 576 };
    const grid = { cols: 120, rows: 40 };
    const r = rig(box);
    const d = driver(r, grid, box);
    d.fitter.refit(r.term, r.host);
    d.settle();
    const first = r.term.options.fontSize;
    for (let i = 0; i < 10; i += 1) {
      d.fitter.refit(r.term, r.host);
      d.settle();
    }
    expect(r.term.options.fontSize).toBe(first);
  });

  it("a verification pass always follows the fit, and stops once the font is stable", () => {
    const box = { w: 1230, h: 576 };
    const grid = { cols: 120, rows: 40 };
    const r = rig(box);
    const d = driver(r, grid, box);
    d.fitter.refit(r.term, r.host);
    expect(d.queue.length).toBe(1);
    d.settle();
    expect(d.queue.length).toBe(0);
    expect(d.results.length).toBeLessThanOrEqual(4);
  });

  it("a newer refit cancels the verification of the older one", () => {
    const box = { w: 1230, h: 576 };
    const grid = { cols: 120, rows: 40 };
    const r = rig(box);
    const d = driver(r, grid, box);
    d.fitter.refit(r.term, r.host);
    const stale = d.queue[0];
    d.fitter.refit(r.term, r.host);
    const before = d.results.length;
    stale();
    expect(d.results.length).toBe(before);
  });

  it("dispose stops pending passes", () => {
    const box = { w: 1230, h: 576 };
    const r = rig(box);
    const d = driver(r, { cols: 120, rows: 40 }, box);
    d.fitter.refit(r.term, r.host);
    const pending = d.queue[0];
    d.fitter.dispose();
    const before = d.results.length;
    pending?.();
    expect(d.results.length).toBe(before);
  });
});
