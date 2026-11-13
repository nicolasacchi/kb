// T17 — the pure scroller core. Every number here is a measured spike
// value; a change to a constant or a rule must fail one of these.

import { describe, expect, it } from "vitest";
import {
  FLING_IDLE_MS,
  FLING_V,
  FONT_STEPS,
  LOCK_PX,
  STOP_V,
  TAU_MS,
  accumulateLines,
  lockAxis,
  momentumStep,
  nearestFontStep,
  pinchFont,
  releaseVelocity,
  stepFont,
  trimSamples,
} from "./scrollerCore";

describe("constants", () => {
  it("carry the measured spike values", () => {
    expect(LOCK_PX).toBe(8);
    expect(TAU_MS).toBe(325);
    expect(FLING_V).toBe(0.15);
    expect(FLING_IDLE_MS).toBe(80);
    expect(STOP_V).toBe(0.02);
    expect([...FONT_STEPS]).toEqual([8, 9, 10, 11, 12, 13, 14, 16, 18, 20, 22, 24]);
  });
});

describe("lockAxis", () => {
  it("is null below the lock distance and picks the dominant axis above it", () => {
    expect(lockAxis(3, 3)).toBeNull();
    expect(lockAxis(7.9, 0)).toBeNull();
    expect(lockAxis(8, 0)).toBe("h");
    expect(lockAxis(0, 8)).toBe("v");
    expect(lockAxis(9, 2)).toBe("h");
    expect(lockAxis(2, 9)).toBe("v");
    // hypot 8.49 is past the lock and the tie goes to vertical.
    expect(lockAxis(6, 6)).toBe("v");
  });
});

describe("accumulateLines", () => {
  it("turns 170 px of 5 px steps into exactly 10 lines at 17 px, carrying the remainder", () => {
    let acc = 0;
    let lines = 0;
    for (let i = 0; i < 34; i += 1) {
      const r = accumulateLines(acc, 5, 17);
      acc = r.acc;
      lines += r.lines;
      // never rounds up per event: the carried remainder stays below one cell.
      expect(Math.abs(acc)).toBeLessThan(17);
    }
    expect(lines).toBe(10);
    expect(acc).toBeCloseTo(0, 9);
  });

  it("carries a sub-cell remainder and scrolls the other way for a negative travel", () => {
    expect(accumulateLines(0, 10, 17)).toEqual({ lines: 0, acc: 10 });
    expect(accumulateLines(10, 10, 17)).toEqual({ lines: 1, acc: 3 });
    expect(accumulateLines(0, -40, 17)).toEqual({ lines: -2, acc: -6 });
  });

  it("does nothing while the cell height is unknown", () => {
    expect(accumulateLines(0, 50, 0)).toEqual({ lines: 0, acc: 50 });
  });
});

describe("releaseVelocity", () => {
  const fast = [
    { t: 1000, y: 500 },
    { t: 1100, y: 740 },
  ];
  it("flings when 240 px in 100 ms ends at release", () => {
    const r = releaseVelocity(fast, 1100);
    expect(r.v).toBeCloseTo(2.4, 9);
    expect(r.fling).toBe(true);
  });
  it("does not fling when the finger rested 120 ms before release", () => {
    const r = releaseVelocity(fast, 1220);
    expect(r.idle).toBe(120);
    expect(r.fling).toBe(false);
  });
  it("does not fling for a slow drag held 200 ms", () => {
    // 10 lines of 17 px over a full second, then a 200 ms hold.
    const slow = [
      { t: 0, y: 0 },
      { t: 1000, y: 170 },
    ];
    expect(releaseVelocity(slow, 1200).fling).toBe(false);
  });
  it("has no velocity from a single sample or a zero time span", () => {
    expect(releaseVelocity([{ t: 5, y: 1 }], 5).v).toBe(0);
    expect(releaseVelocity([], 5).fling).toBe(false);
  });
});

describe("trimSamples", () => {
  it("drops samples older than 100 ms but never below two", () => {
    const s = [
      { t: 0, y: 0 },
      { t: 50, y: 1 },
      { t: 150, y: 2 },
      { t: 200, y: 3 },
    ];
    trimSamples(s, 200);
    expect(s.map((x) => x.t)).toEqual([150, 200]);
    const lone = [
      { t: 0, y: 0 },
      { t: 10, y: 1 },
    ];
    trimSamples(lone, 10_000);
    expect(lone).toHaveLength(2);
  });
});

describe("momentumStep", () => {
  it("decays to v0/e after one time constant", () => {
    expect(Math.abs(momentumStep(2.4, 325) - 2.4 / Math.E)).toBeLessThan(1e-9);
  });
  it("travels far more than the finger did (analytic bound v0*tau = 780 px)", () => {
    let v = 2.4;
    let travel = 0;
    let frames = 0;
    while (Math.abs(v) > STOP_V && frames < 10_000) {
      v = momentumStep(v, 16.7);
      travel += v * 16.7;
      frames += 1;
    }
    expect(travel).toBeGreaterThan(700);
    expect(travel).toBeLessThan(800);
    expect(travel).toBeGreaterThan(240);
  });
});

describe("pinchFont", () => {
  it("snaps to the nearest step", () => {
    expect(pinchFont(12, 1.6)).toBe(20);
    expect(pinchFont(20, 0.3)).toBe(8);
    expect(pinchFont(12, 1.0)).toBe(12);
  });
  it("always lands on a FONT_STEPS member", () => {
    for (const start of FONT_STEPS) {
      for (let scale = 0.1; scale < 4; scale += 0.07) {
        expect(FONT_STEPS as readonly number[]).toContain(pinchFont(start, scale));
      }
    }
  });
  it("breaks a tie toward the smaller step", () => {
    expect(nearestFontStep(15)).toBe(14);
  });
});

describe("stepFont", () => {
  it("moves one step and clamps at the ends", () => {
    expect(stepFont(12, 1)).toBe(13);
    expect(stepFont(12, -1)).toBe(11);
    expect(stepFont(24, 1)).toBe(24);
    expect(stepFont(8, -1)).toBe(8);
    expect(stepFont(14, 1)).toBe(16);
  });
});
