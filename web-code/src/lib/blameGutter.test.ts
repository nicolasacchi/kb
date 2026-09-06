import { describe, expect, it } from "vitest";
import type { AttributionOut, BlameRegion } from "../api/types";
import { buildLineDots, distinctRegionShas, regionCoveringLine } from "./blameGutter";

function region(overrides: Partial<BlameRegion> = {}): BlameRegion {
  return {
    sha: "a".repeat(40),
    orig_start: 1,
    final_start: 1,
    count: 1,
    author: "Ada",
    author_mail: "ada@example.com",
    author_time: 1_700_000_000,
    subject: "fix it",
    previous_sha: null,
    previous_filename: null,
    filename: "lib.rs",
    boundary: false,
    ...overrides,
  };
}

function attribution(overrides: Partial<AttributionOut> = {}): AttributionOut {
  return { confidence: "exact", via: "by-commit", ...overrides };
}

describe("regionCoveringLine", () => {
  it("finds the region whose final_start..final_start+count-1 covers the line", () => {
    const regions = [region({ final_start: 1, count: 3 }), region({ final_start: 4, count: 2 })];
    expect(regionCoveringLine(regions, 5)).toBe(regions[1]);
    expect(regionCoveringLine(regions, 2)).toBe(regions[0]);
  });

  it("returns undefined for a line out of range for every region", () => {
    const regions = [region({ final_start: 1, count: 3 })];
    expect(regionCoveringLine(regions, 10)).toBeUndefined();
    expect(regionCoveringLine(regions, 0)).toBeUndefined();
  });
});

describe("distinctRegionShas", () => {
  it("dedupes, preserving first-seen order", () => {
    const shas = distinctRegionShas([
      region({ sha: "sha1" }),
      region({ sha: "sha2" }),
      region({ sha: "sha1" }),
    ]);
    expect(shas).toEqual(["sha1", "sha2"]);
  });

  it("returns an empty array for no regions", () => {
    expect(distinctRegionShas([])).toEqual([]);
  });
});

describe("buildLineDots", () => {
  it("omits a dot for a region whose attribution hasn't resolved yet", () => {
    const regions = [region({ sha: "sha1", final_start: 1, count: 2 })];
    const dots = buildLineDots(regions, new Map());
    expect(dots.size).toBe(0);
  });

  it("omits a dot for confidence: none (honest absence, not a placeholder dot)", () => {
    const regions = [region({ sha: "sha1", final_start: 1, count: 2 })];
    const map = new Map([["sha1", attribution({ confidence: "none", via: "no-match" })]]);
    expect(buildLineDots(regions, map).size).toBe(0);
  });

  it("marks trailer and exact confidence solid", () => {
    for (const confidence of ["trailer", "exact"] as const) {
      const regions = [region({ sha: "sha1", final_start: 5, count: 1 })];
      const map = new Map([["sha1", attribution({ confidence })]]);
      const dots = buildLineDots(regions, map);
      expect(dots.get(5)?.solid).toBe(true);
    }
  });

  it("marks fuzzy confidence outlined (not solid)", () => {
    const regions = [region({ sha: "sha1", final_start: 5, count: 1 })];
    const map = new Map([["sha1", attribution({ confidence: "fuzzy" })]]);
    expect(buildLineDots(regions, map).get(5)?.solid).toBe(false);
  });

  it("spans every line the region covers", () => {
    const regions = [region({ sha: "sha1", final_start: 10, count: 4 })];
    const map = new Map([["sha1", attribution()]]);
    const dots = buildLineDots(regions, map);
    expect([...dots.keys()].sort((a, b) => a - b)).toEqual([10, 11, 12, 13]);
  });

  it("prefers display_name over the commit subject for the hover label", () => {
    const regions = [region({ sha: "sha1", final_start: 1, count: 1, subject: "subject line" })];
    const map = new Map([["sha1", attribution({ display_name: "fixed the gizmo" })]]);
    expect(buildLineDots(regions, map).get(1)?.label).toBe("fixed the gizmo · exact");
  });

  it("falls back to the commit subject when no display_name is present", () => {
    const regions = [region({ sha: "sha1", final_start: 1, count: 1, subject: "subject line" })];
    const map = new Map([["sha1", attribution({ display_name: undefined })]]);
    expect(buildLineDots(regions, map).get(1)?.label).toBe("subject line · exact");
  });
});
