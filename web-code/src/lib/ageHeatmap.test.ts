import { describe, expect, it } from "vitest";
import { buildAgeLineBuckets } from "./ageHeatmap";
import type { BlameRegion } from "../api/types";

function region(sha: string, final_start: number, count: number, author_time: number): BlameRegion {
  return {
    sha,
    orig_start: final_start,
    final_start,
    count,
    author: "Test",
    author_mail: "t@example.com",
    author_time,
    subject: `commit ${sha}`,
    previous_sha: null,
    previous_filename: null,
    filename: "a.rs",
    boundary: false,
  };
}

describe("buildAgeLineBuckets", () => {
  it("returns an empty map for zero regions", () => {
    expect(buildAgeLineBuckets([])).toEqual(new Map());
  });

  it("buckets every line to 0 when every region shares one author_time", () => {
    const regions = [region("a", 1, 3, 1000), region("b", 4, 2, 1000)];
    const buckets = buildAgeLineBuckets(regions);
    for (let line = 1; line <= 5; line++) {
      expect(buckets.get(line)?.bucket).toBe(0);
    }
  });

  it("assigns the newest region bucket 0 and the oldest the last bucket", () => {
    const regions = [
      region("newest", 1, 1, 500), // newest
      region("oldest", 2, 1, 0), // oldest
    ];
    const buckets = buildAgeLineBuckets(regions);
    expect(buckets.get(1)?.bucket).toBe(0);
    expect(buckets.get(2)?.bucket).toBe(4);
  });

  it("spreads five regions evenly spanning the full range across all five buckets", () => {
    const regions = [
      region("r0", 1, 1, 400), // newest
      region("r1", 2, 1, 300),
      region("r2", 3, 1, 200),
      region("r3", 4, 1, 100),
      region("r4", 5, 1, 0), // oldest
    ];
    const buckets = buildAgeLineBuckets(regions);
    expect([1, 2, 3, 4, 5].map((l) => buckets.get(l)?.bucket)).toEqual([0, 1, 2, 3, 4]);
  });

  it("covers every line in a region's [final_start, final_start+count) span", () => {
    const regions = [region("a", 10, 3, 1)];
    const buckets = buildAgeLineBuckets(regions);
    expect(buckets.has(9)).toBe(false);
    expect(buckets.has(10)).toBe(true);
    expect(buckets.has(11)).toBe(true);
    expect(buckets.has(12)).toBe(true);
    expect(buckets.has(13)).toBe(false);
  });

  it("carries the region's own author_time alongside the bucket", () => {
    const regions = [region("a", 1, 1, 12345)];
    const buckets = buildAgeLineBuckets(regions);
    expect(buckets.get(1)?.author_time).toBe(12345);
  });
});
