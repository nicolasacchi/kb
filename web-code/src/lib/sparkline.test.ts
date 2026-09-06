import { describe, expect, it } from "vitest";
import {
  churnSeries,
  sparkTooltip,
  sparklinePath,
  weekLabel,
  type SparkBucket,
} from "./sparkline";

describe("sparklinePath", () => {
  it("returns empty for no values", () => {
    expect(sparklinePath([], 40, 12)).toBe("");
  });

  it("draws a flat midline for constant series", () => {
    const d = sparklinePath([0, 0, 0], 40, 12, 2);
    expect(d.startsWith("M ")).toBe(true);
    // y = mid of [2, 10] = 6
    expect(d).toContain("0,6");
    expect(d).toContain("40,6");
  });

  it("scales min→max across the pad band", () => {
    const d = sparklinePath([0, 10], 100, 20, 0);
    // first at top of band? 0 → min → y=height (bottom); 10 → max → y=0
    expect(d).toMatch(/^M 0,20 L 100,0$/);
  });

  it("handles a single point at mid-x", () => {
    const d = sparklinePath([5], 40, 12, 0);
    expect(d).toBe("M 20,6");
  });
});

describe("churnSeries / weekLabel / sparkTooltip", () => {
  const buckets: SparkBucket[] = [
    { week_start_unix: 1_704_067_200, commits: 2, churn: 40, authors: 1 }, // 2024-01-01-ish
    { week_start_unix: 1_704_672_000, commits: 1, churn: 10, authors: 2 },
  ];

  it("maps churn", () => {
    expect(churnSeries(buckets)).toEqual([40, 10]);
  });

  it("formats UTC week label", () => {
    expect(weekLabel(1_704_067_200)).toMatch(/^\d{4}-\d{2}-\d{2}$/);
  });

  it("builds tooltip with counts", () => {
    const t = sparkTooltip(buckets[0]!);
    expect(t).toContain("commits 2");
    expect(t).toContain("churn 40");
    expect(t).toContain("authors 1");
  });
});
