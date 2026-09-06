import { describe, expect, it } from "vitest";
import {
  dispositionSegments,
  formatAnalyticsLatency,
  formatAnalyticsRate,
  groupBySeverity,
} from "./reviewAnalytics";
import type { AnalyticsSeverityDispositionCell } from "../api/types";

describe("formatAnalyticsRate", () => {
  it("null -> em-dash, never a fabricated 0%", () => {
    expect(formatAnalyticsRate(null)).toBe("—");
  });
  it("rounds to a whole percent", () => {
    expect(formatAnalyticsRate(0.5)).toBe("50%");
    expect(formatAnalyticsRate(1)).toBe("100%");
    expect(formatAnalyticsRate(0)).toBe("0%");
    expect(formatAnalyticsRate(0.333)).toBe("33%");
  });
});

describe("formatAnalyticsLatency", () => {
  it("null -> em-dash, never a fabricated 0s", () => {
    expect(formatAnalyticsLatency(null)).toBe("—");
  });
  it("negative (should-never-happen) -> em-dash, not a nonsense duration", () => {
    expect(formatAnalyticsLatency(-5)).toBe("—");
  });
  it("buckets into human units", () => {
    expect(formatAnalyticsLatency(0)).toBe("0s");
    expect(formatAnalyticsLatency(45)).toBe("45s");
    expect(formatAnalyticsLatency(90)).toBe("2m");
    expect(formatAnalyticsLatency(3 * 3600)).toBe("3h");
    expect(formatAnalyticsLatency(3 * 86_400)).toBe("3d");
  });
});

function cell(severity: string, disposition: string, count: number): AnalyticsSeverityDispositionCell {
  return { severity: severity as never, disposition, count };
}

describe("groupBySeverity", () => {
  it("buckets by severity, preserving per-severity cell order", () => {
    const cells = [
      cell("blocker", "agree", 3),
      cell("blocker", "dispute", 1),
      cell("concern", "agree", 2),
    ];
    const grouped = groupBySeverity(cells);
    expect(grouped.get("blocker")).toEqual([cell("blocker", "agree", 3), cell("blocker", "dispute", 1)]);
    expect(grouped.get("concern")).toEqual([cell("concern", "agree", 2)]);
  });
});

describe("dispositionSegments", () => {
  it("computes percentages of the ROW's own total", () => {
    const cells = [cell("blocker", "agree", 3), cell("blocker", "dispute", 1)];
    const segs = dispositionSegments(cells);
    expect(segs).toEqual([
      { disposition: "agree", count: 3, pct: 75 },
      { disposition: "dispute", count: 1, pct: 25 },
    ]);
  });

  it("an all-zero row degrades to 0% everywhere, never NaN/Infinity", () => {
    const cells = [cell("ok", "agree", 0), cell("ok", "dispute", 0)];
    const segs = dispositionSegments(cells);
    expect(segs.every((s) => s.pct === 0)).toBe(true);
    expect(segs.some((s) => Number.isNaN(s.pct))).toBe(false);
  });

  it("zero-count cells still get a segment (never omitted, DOM stays stable)", () => {
    const cells = [cell("ok", "agree", 5), cell("ok", "waive", 0)];
    const segs = dispositionSegments(cells);
    expect(segs).toHaveLength(2);
    expect(segs[1]).toEqual({ disposition: "waive", count: 0, pct: 0 });
  });
});
