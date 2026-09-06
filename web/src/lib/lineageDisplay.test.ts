import { describe, expect, it } from "vitest";
import { planLineageDisplay } from "./lineageDisplay";

describe("planLineageDisplay", () => {
  it("shows everything and truncates nothing for a short chain", () => {
    const plan = planLineageDisplay(["a"], ["b"], 2);
    expect(plan).toEqual({
      shownOlder: ["a"],
      olderTruncated: 0,
      shownNewer: ["b"],
      newerTruncated: 0,
    });
  });

  it("truncates a long chain to maxPerSide, closest-to-start first, with a count", () => {
    const plan = planLineageDisplay(["a1", "a2", "a3", "a4"], [], 2);
    expect(plan.shownOlder).toEqual(["a1", "a2"]);
    expect(plan.olderTruncated).toBe(2);
  });

  it("truncates each side independently", () => {
    const plan = planLineageDisplay(["a1", "a2", "a3"], ["b1"], 2);
    expect(plan.shownOlder).toEqual(["a1", "a2"]);
    expect(plan.olderTruncated).toBe(1);
    expect(plan.shownNewer).toEqual(["b1"]);
    expect(plan.newerTruncated).toBe(0);
  });

  it("handles empty chains on both sides", () => {
    const plan = planLineageDisplay([], [], 2);
    expect(plan).toEqual({
      shownOlder: [],
      olderTruncated: 0,
      shownNewer: [],
      newerTruncated: 0,
    });
  });

  it("never grows past maxPerSide regardless of input size — the hairball guard", () => {
    const huge = Array.from({ length: 200 }, (_, i) => `n${i}`);
    const plan = planLineageDisplay(huge, huge, 2);
    expect(plan.shownOlder.length).toBe(2);
    expect(plan.shownNewer.length).toBe(2);
    expect(plan.olderTruncated).toBe(198);
    expect(plan.newerTruncated).toBe(198);
  });
});
