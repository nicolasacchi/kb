// PHIN1 - cursor-follow geometry: the visible band for both soft-keyboard resize
// variants, the minimal plan, and the manual-pan gate. Node environment.

import { describe, expect, it } from "vitest";
import { SUSPEND_MS, createFollowGate, planFollow, visibleBand, type Rect, type VV } from "./cursorFollow";

const rect = (left: number, top: number, right: number, bottom: number): Rect => ({ left, top, right, bottom });

// A 400 x 600 Watch box at (0, 100): client rows 100..700.
const WATCH = rect(0, 100, 400, 700);

describe("visibleBand", () => {
  const rows: Array<[string, Rect, VV | null, Rect]> = [
    // Firefox Android: the layout viewport shrinks with the keyboard, so the Watch box is already short.
    ["Firefox layout-resize (offsetTop 0)", rect(0, 100, 400, 400), { offsetTop: 0, offsetLeft: 0, width: 400, height: 450 }, rect(0, 100, 400, 400)],
    // Chrome Android: the Watch box keeps its size, the visual viewport is shorter.
    ["Chrome visual-resize, offsetTop 0", WATCH, { offsetTop: 0, offsetLeft: 0, width: 400, height: 450 }, rect(0, 100, 400, 450)],
    ["Chrome visual-resize, offsetTop > 0", WATCH, { offsetTop: 120, offsetLeft: 0, width: 400, height: 450 }, rect(0, 120, 400, 570)],
    ["pinch-zoomed horizontally", WATCH, { offsetTop: 0, offsetLeft: 50, width: 300, height: 800 }, rect(50, 100, 350, 700)],
    ["no visualViewport API", WATCH, null, WATCH],
  ];
  for (const [name, watch, vv, want] of rows) {
    it(name, () => {
      expect(visibleBand(watch, vv)).toEqual(want);
    });
  }

  it("an empty intersection is a zero-height rect at the Watch top, never NaN or negative", () => {
    const b = visibleBand(WATCH, { offsetTop: 900, offsetLeft: 0, width: 400, height: 300 });
    expect(b).toEqual(rect(0, 100, 400, 100));
    const n = visibleBand(WATCH, { offsetTop: Number.NaN, offsetLeft: 0, width: 400, height: 300 });
    expect(n.bottom - n.top).toBe(0);
    for (const v of Object.values(n)) expect(Number.isNaN(v)).toBe(false);
    const w = visibleBand(WATCH, { offsetTop: 0, offsetLeft: 500, width: 100, height: 300 });
    expect(w.bottom - w.top).toBe(0);
    expect(w.right).toBeGreaterThanOrEqual(w.left);
  });
});

describe("planFollow", () => {
  const CW = 10;
  const CH = 20;
  // The band is the Chrome visual-resize one: client rows 100..450 and columns 0..400.
  const BAND = rect(0, 100, 400, 450);
  const cell = (col: number, top: number): Rect => rect(col * CW, top, col * CW + CW, top + CH);
  const plan = (cursor: Rect, band = BAND, extra: Partial<Parameters<typeof planFollow>[0]> = {}) =>
    planFollow({ cursor, band, cellW: CW, cellH: CH, ...extra });

  it("a cursor already inside the band, margins included, needs nothing", () => {
    expect(plan(cell(10, 200))).toEqual({ dx: 0, dy: 0 });
  });

  it("a cursor under the keyboard needs dy > 0 so that its row plus one margin row sits at the bottom", () => {
    // Cell 540..560 is below the band bottom 450: bottom + 1 row margin = 580.
    expect(plan(cell(10, 540))).toEqual({ dx: 0, dy: 580 - 450 });
  });

  it("past the right edge needs dx > 0 with the 3-column margin", () => {
    // Cell columns 395..405 -> right edge 405 + 30 = 435 over the band right 400.
    const p = plan(rect(395, 200, 405, 220));
    expect(p.dx).toBe(35);
    expect(p.dy).toBe(0);
  });

  it("left of the pan needs dx < 0 with the 3-column margin", () => {
    // Cell left edge 5 - 30 = -25 under the band left 0.
    expect(plan(rect(5, 200, 15, 220)).dx).toBe(-25);
  });

  it("a cursor above the band (clipped at the top) needs dy < 0", () => {
    // Cell top 60 - 1 row margin = 40 over the band top 100.
    expect(plan(cell(10, 60)).dy).toBe(-60);
  });

  it("a band shorter than a cell plus margins: the cursor row's bottom edge wins", () => {
    const short = rect(0, 100, 400, 130); // 30 px: a cell (20) fits, a cell plus a row each side (60) does not
    expect(plan(cell(10, 200), short).dy).toBe(220 - 130);
    // already inside the short band: stays put
    expect(plan(cell(10, 105), short).dy).toBe(0);
  });

  it("honours explicit margins", () => {
    expect(plan(cell(10, 440), BAND, { marginRows: 0 }).dy).toBe(460 - 450);
    expect(plan(rect(395, 200, 405, 220), BAND, { marginCols: 0 }).dx).toBe(5);
  });

  it("a degenerate band or a non-numeric input moves nothing", () => {
    expect(plan(cell(10, 540), rect(0, 100, 400, 100))).toEqual({ dx: 0, dy: 0 });
    expect(plan(cell(10, 540), rect(0, 100, Number.NaN, 450))).toEqual({ dx: 0, dy: 0 });
    for (const bad of [0, -3, Number.NaN, Number.POSITIVE_INFINITY]) {
      expect(planFollow({ cursor: cell(10, 540), band: BAND, cellW: bad, cellH: CH })).toEqual({ dx: 0, dy: 0 });
      expect(planFollow({ cursor: cell(10, 540), band: BAND, cellW: CW, cellH: bad })).toEqual({ dx: 0, dy: 0 });
    }
  });
});

describe("createFollowGate", () => {
  function gate() {
    const clock = { t: 5000 };
    return { clock, g: createFollowGate({ now: () => clock.t }) };
  }

  it("is open until the first manual note", () => {
    expect(gate().g.allowed()).toBe(true);
  });

  it("blocks until exactly 2000 ms after the note", () => {
    const { clock, g } = gate();
    g.noteManual();
    clock.t += SUSPEND_MS - 1;
    expect(g.allowed()).toBe(false);
    clock.t += 1;
    expect(g.allowed()).toBe(true);
  });

  it("a second note restarts the window from the LAST note", () => {
    const { clock, g } = gate();
    g.noteManual();
    clock.t += 1500;
    g.noteManual();
    clock.t += 1999;
    expect(g.allowed()).toBe(false);
    clock.t += 1;
    expect(g.allowed()).toBe(true);
  });

  it("reset() re-allows at once", () => {
    const { g } = gate();
    g.noteManual();
    expect(g.allowed()).toBe(false);
    g.reset();
    expect(g.allowed()).toBe(true);
  });

  it("honours a custom suspend time", () => {
    const clock = { t: 0 };
    const g = createFollowGate({ now: () => clock.t, suspendMs: 50 });
    g.noteManual();
    clock.t = 49;
    expect(g.allowed()).toBe(false);
    clock.t = 50;
    expect(g.allowed()).toBe(true);
  });
});
