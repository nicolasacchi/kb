import { describe, expect, it } from "vitest";
import {
  buildQuadrantPoints,
  quadrantCounts,
  type QuadrantInput,
  type QuadrantThresholds,
} from "./recallQuadrant";

const NOW = 1_700_000_000;
const DAY = 86_400;
// FIX2 — the wire-supplied constants (kb_core::triage::
// HIGH_SALIENCE_THRESHOLD/DORMANT_DAYS on GET /api/memory/triage), passed
// explicitly at every call site now that `recallQuadrant.ts` no longer
// hardcodes them.
const THRESHOLDS: QuadrantThresholds = { highSalienceThreshold: 0.7, dormantDays: 60 };

function row(overrides: Partial<QuadrantInput> = {}): QuadrantInput {
  return {
    kb: "notes",
    id: "aaaaaaaaaaaa",
    title: "t",
    sourceRelative: "t.html",
    salience: 0.5,
    recallCount: 0,
    lastRecalledAt: null,
    ...overrides,
  };
}

describe("buildQuadrantPoints", () => {
  it("classifies high-salience + never-recalled as dead-weight", () => {
    const [p] = buildQuadrantPoints(
      [row({ salience: 0.9, lastRecalledAt: null })],
      NOW,
      THRESHOLDS,
    );
    expect(p.quadrant).toBe("dead-weight");
    expect(p.daysSinceRecall).toBeNull();
  });

  it("classifies high-salience + long-idle as dead-weight, not just never-recalled", () => {
    const [p] = buildQuadrantPoints(
      [row({ salience: 0.9, lastRecalledAt: NOW - 90 * DAY })],
      NOW,
      THRESHOLDS,
    );
    expect(p.quadrant).toBe("dead-weight");
    expect(p.daysSinceRecall).toBeCloseTo(90, 5);
  });

  it("classifies low-salience + frequently-recalled as mis-scored", () => {
    const [p] = buildQuadrantPoints(
      [row({ salience: 0.1, recallCount: 20, lastRecalledAt: NOW - 1 * DAY })],
      NOW,
      THRESHOLDS,
    );
    expect(p.quadrant).toBe("mis-scored");
  });

  it("classifies high-salience + recently-recalled as healthy-active", () => {
    const [p] = buildQuadrantPoints(
      [row({ salience: 0.9, lastRecalledAt: NOW - 1 * DAY })],
      NOW,
      THRESHOLDS,
    );
    expect(p.quadrant).toBe("healthy-active");
  });

  it("classifies low-salience + idle as healthy-idle (the boring, expected case)", () => {
    const [p] = buildQuadrantPoints(
      [row({ salience: 0.2, lastRecalledAt: NOW - 90 * DAY })],
      NOW,
      THRESHOLDS,
    );
    expect(p.quadrant).toBe("healthy-idle");
  });

  it("clamps a future last_recalled_at (clock skew) to zero days, not negative", () => {
    const [p] = buildQuadrantPoints([row({ lastRecalledAt: NOW + 1000 })], NOW, THRESHOLDS);
    expect(p.daysSinceRecall).toBe(0);
  });

  it("preserves every input row's identity fields on the output point", () => {
    const [p] = buildQuadrantPoints(
      [row({ kb: "g", id: "deadbeef0000", title: "Some Fact" })],
      NOW,
      THRESHOLDS,
    );
    expect(p.kb).toBe("g");
    expect(p.id).toBe("deadbeef0000");
    expect(p.title).toBe("Some Fact");
  });

  it("uses the SUPPLIED thresholds, not any hardcoded default", () => {
    // A salience of 0.5 is "high" only under a lowered threshold — proves
    // the classification genuinely reads the parameter, not a module const.
    const lowered: QuadrantThresholds = { highSalienceThreshold: 0.4, dormantDays: 60 };
    const [p] = buildQuadrantPoints(
      [row({ salience: 0.5, lastRecalledAt: null })],
      NOW,
      lowered,
    );
    expect(p.quadrant).toBe("dead-weight");
  });
});

describe("quadrantCounts", () => {
  it("tallies every point into exactly one quadrant bucket", () => {
    const points = buildQuadrantPoints(
      [
        row({ id: "a", salience: 0.9, lastRecalledAt: null }), // dead-weight
        row({ id: "b", salience: 0.1, recallCount: 5, lastRecalledAt: NOW - DAY }), // mis-scored
        row({ id: "c", salience: 0.9, lastRecalledAt: NOW - DAY }), // healthy-active
        row({ id: "d", salience: 0.2, lastRecalledAt: NOW - 90 * DAY }), // healthy-idle
      ],
      NOW,
      THRESHOLDS,
    );
    const counts = quadrantCounts(points);
    expect(counts).toEqual({
      "dead-weight": 1,
      "mis-scored": 1,
      "healthy-active": 1,
      "healthy-idle": 1,
    });
  });

  it("returns all-zero buckets for an empty input, not a missing key", () => {
    expect(quadrantCounts([])).toEqual({
      "dead-weight": 0,
      "mis-scored": 0,
      "healthy-active": 0,
      "healthy-idle": 0,
    });
  });
});
