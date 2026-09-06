import { describe, expect, it } from "vitest";
import {
  decayHalfLifeDays,
  decayHalfLifeLabel,
  decayValueAt,
  floorState,
  floorStateLabel,
  projectDecayCurve,
  type DecayCurveInput,
} from "./decayProjection";

const DECAY_SLOW = 0.01;
const DECAY_FAST = 0.1;

describe("decayValueAt", () => {
  it("at t=0 with age=0 equals raw salience (undecayed)", () => {
    const input: DecayCurveInput = { salience: 0.8, ageDaysNow: 0, decayK: DECAY_SLOW };
    expect(decayValueAt(input, 0)).toBeCloseTo(0.8, 6);
  });

  it("decays monotonically forward in time", () => {
    const input: DecayCurveInput = { salience: 0.8, ageDaysNow: 0, decayK: DECAY_FAST };
    const v0 = decayValueAt(input, 0);
    const v10 = decayValueAt(input, 10);
    const v20 = decayValueAt(input, 20);
    expect(v10).toBeLessThan(v0);
    expect(v20).toBeLessThan(v10);
  });

  it("stability >= 1 caps at the raw salience, never exceeds it", () => {
    const input: DecayCurveInput = {
      salience: 0.5,
      ageDaysNow: 0,
      decayK: DECAY_SLOW,
      stability: 3,
    };
    // At t=0, base decay is 1.0; ×3 stability would overshoot to 1.5 —
    // must be capped at the raw salience (0.5), matching
    // `rerank_with_policy_scored`'s `(base_decay * s).min(1.0)` cap.
    expect(decayValueAt(input, 0)).toBeCloseTo(0.5, 6);
  });

  it("missing stability behaves exactly like stability=1 (a no-op)", () => {
    const withNone: DecayCurveInput = { salience: 0.6, ageDaysNow: 5, decayK: DECAY_FAST };
    const withOne: DecayCurveInput = { ...withNone, stability: 1 };
    expect(decayValueAt(withNone, 20)).toBeCloseTo(decayValueAt(withOne, 20), 10);
  });
});

describe("projectDecayCurve", () => {
  it("returns samples+1 points spanning [0, horizonDays]", () => {
    const pts = projectDecayCurve({
      salience: 0.5,
      ageDaysNow: 0,
      decayK: DECAY_SLOW,
      horizonDays: 90,
      samples: 9,
    });
    expect(pts).toHaveLength(10);
    expect(pts[0].t).toBe(0);
    expect(pts[pts.length - 1].t).toBe(90);
  });

  it("defaults to a 90-day horizon with 30 samples", () => {
    const pts = projectDecayCurve({ salience: 0.5, ageDaysNow: 0, decayK: DECAY_SLOW });
    expect(pts).toHaveLength(31);
    expect(pts[pts.length - 1].t).toBe(90);
  });

  it("is non-increasing across the whole curve", () => {
    const pts = projectDecayCurve({
      salience: 0.9,
      ageDaysNow: 2,
      decayK: DECAY_FAST,
      stability: 2,
    });
    for (let i = 1; i < pts.length; i++) {
      expect(pts[i].y).toBeLessThanOrEqual(pts[i - 1].y + 1e-9);
    }
  });
});

// === MI-W4.1(revision) — `floorState` / `floorStateLabel` ==================
//
// Ground truth these pin: raw salience is CONSTANT, so a memory's
// `FloorState` never changes with the passage of time (absent an explicit
// salience edit). There is deliberately no "age" parameter anywhere here —
// a caller cannot even construct the false "this will drop in Nd" claim
// the deleted `findCrossing` allowed.

describe("floorState", () => {
  it("pinned short-circuits before any salience comparison", () => {
    expect(floorState(0.0, 0.9, true)).toEqual({ kind: "pinned" });
  });

  it("a null floor (Loose policy) is always no-floor", () => {
    expect(floorState(0.01, null, false)).toEqual({ kind: "no-floor" });
  });

  it("above the floor when salience exceeds it", () => {
    expect(floorState(0.4, 0.15, false)).toEqual({ kind: "above", salience: 0.4, floor: 0.15 });
  });

  it("below the floor when salience is at or under it — a CURRENT state, not a future one", () => {
    expect(floorState(0.15, 0.15, false)).toEqual({ kind: "below", salience: 0.15, floor: 0.15 });
    expect(floorState(0.1, 0.15, false)).toEqual({ kind: "below", salience: 0.1, floor: 0.15 });
  });

  it("a high-salience memory is ABOVE the floor regardless of how the caller frames 'time' — there is no age input to even try", () => {
    // The whole point of this revision: you cannot pass an age/decay-rate
    // to `floorState` to make a high-salience memory report `below`,
    // because the function signature doesn't accept one.
    const a = floorState(0.9, 0.15, false);
    const b = floorState(0.9, 0.15, false);
    expect(a).toEqual(b);
    expect(a.kind).toBe("above");
  });
});

describe("floorStateLabel", () => {
  it("renders a distinct, honest label per state — never a predicted date", () => {
    expect(floorStateLabel({ kind: "pinned" })).toMatch(/pinned/);
    expect(floorStateLabel({ kind: "no-floor" })).toMatch(/loose/);
    expect(floorStateLabel({ kind: "above", salience: 0.4, floor: 0.15 })).toBe(
      "salience 0.40 — above the 0.15 floor",
    );
    expect(floorStateLabel({ kind: "below", salience: 0.1, floor: 0.15 })).toBe(
      "salience 0.10 — below the 0.15 floor, excluded from recall now",
    );
  });
});

// === MI-W4.1(revision) — `decayHalfLifeDays` / `decayHalfLifeLabel` =======

describe("decayHalfLifeDays", () => {
  it("matches the Rust decay_half_life_days golden fixture for both named buckets", () => {
    // Mirrors kb-core's `decay_half_life_matches_the_two_named_buckets` —
    // ln(2)/0.01 ≈ 69.31d (slow), ln(2)/0.1 ≈ 6.93d (fast).
    expect(decayHalfLifeDays(DECAY_SLOW)).toBeCloseTo(69.3147, 3);
    expect(decayHalfLifeDays(DECAY_FAST)).toBeCloseTo(6.93147, 3);
  });

  it("is infinite for a non-positive rate", () => {
    expect(decayHalfLifeDays(0)).toBe(Infinity);
    expect(decayHalfLifeDays(-1)).toBe(Infinity);
  });

  it("does not depend on salience, age, or stability — there is no such parameter", () => {
    expect(decayHalfLifeDays(DECAY_SLOW)).toBe(decayHalfLifeDays(DECAY_SLOW));
  });
});

describe("decayHalfLifeLabel", () => {
  it("renders the rounded half-life for the two named buckets", () => {
    expect(decayHalfLifeLabel(DECAY_SLOW)).toBe("score halves every ~69d");
    expect(decayHalfLifeLabel(DECAY_FAST)).toBe("score halves every ~7d");
  });

  it("says so for a non-decaying rate", () => {
    expect(decayHalfLifeLabel(0)).toBe("score never decays");
  });
});
