import { describe, expect, it } from "vitest";
import {
  decayBucketFillColor,
  hasMemoryPoints,
  mergeAtlasEdges,
  MEMORY_COLOR_FALLBACK,
  salienceFillColor,
  supersedeEdges,
} from "./atlasMemory";

describe("hasMemoryPoints", () => {
  it("is false for an empty point set", () => {
    expect(hasMemoryPoints([])).toBe(false);
  });

  it("is false when every point has all four memory fields absent (a non-memory-scoped kb)", () => {
    expect(
      hasMemoryPoints([
        { salience: undefined, decay_bucket: undefined, pinned: undefined, forgotten: undefined, supersedes: undefined },
        { salience: undefined, decay_bucket: undefined, pinned: undefined, forgotten: undefined, supersedes: undefined },
      ]),
    ).toBe(false);
  });

  it("is true when even one point carries one memory field", () => {
    expect(
      hasMemoryPoints([
        { salience: undefined, decay_bucket: undefined, pinned: undefined, forgotten: undefined, supersedes: undefined },
        { salience: 0.4, decay_bucket: undefined, pinned: undefined, forgotten: undefined, supersedes: undefined },
      ]),
    ).toBe(true);
  });

  it("is true off `pinned: false` alone (a real memory-scope value, not absence)", () => {
    expect(
      hasMemoryPoints([
        { salience: undefined, decay_bucket: undefined, pinned: false, forgotten: undefined, supersedes: undefined },
      ]),
    ).toBe(true);
  });
});

describe("salienceFillColor", () => {
  it("falls back to neutral grey for null/undefined/NaN", () => {
    expect(salienceFillColor(null)).toBe(MEMORY_COLOR_FALLBACK);
    expect(salienceFillColor(undefined)).toBe(MEMORY_COLOR_FALLBACK);
    expect(salienceFillColor(Number.NaN)).toBe(MEMORY_COLOR_FALLBACK);
  });

  it("is deterministic and distinct at the extremes", () => {
    const low = salienceFillColor(0);
    const high = salienceFillColor(1);
    expect(low).not.toBe(high);
    expect(salienceFillColor(0)).toBe(low);
    expect(salienceFillColor(1)).toBe(high);
  });

  it("clamps out-of-range salience rather than throwing", () => {
    expect(() => salienceFillColor(5)).not.toThrow();
    expect(() => salienceFillColor(-2)).not.toThrow();
  });
});

describe("decayBucketFillColor", () => {
  it("falls back to neutral grey for an absent or unrecognised bucket", () => {
    expect(decayBucketFillColor(undefined)).toBe(MEMORY_COLOR_FALLBACK);
    expect(decayBucketFillColor(null)).toBe(MEMORY_COLOR_FALLBACK);
    expect(decayBucketFillColor("")).toBe(MEMORY_COLOR_FALLBACK);
    expect(decayBucketFillColor("glacial")).toBe(MEMORY_COLOR_FALLBACK);
  });

  it("maps the two known buckets to distinct, stable colors", () => {
    const slow = decayBucketFillColor("slow");
    const fast = decayBucketFillColor("fast");
    expect(slow).not.toBe(fast);
    expect(slow).not.toBe(MEMORY_COLOR_FALLBACK);
    expect(fast).not.toBe(MEMORY_COLOR_FALLBACK);
  });
});

describe("supersedeEdges", () => {
  it("is empty for an empty point set", () => {
    expect(supersedeEdges([])).toEqual([]);
  });

  it("emits one edge per point with a supersede target present in the set", () => {
    const points = [
      { id: "a", supersedes: "b" },
      { id: "b", supersedes: undefined },
      { id: "c", supersedes: "a" },
    ];
    expect(supersedeEdges(points)).toEqual([
      { src: "a", dst: "b" },
      { src: "c", dst: "a" },
    ]);
  });

  it("drops a dangling supersede target not present in the point set", () => {
    const points = [{ id: "a", supersedes: "ghost" }];
    expect(supersedeEdges(points)).toEqual([]);
  });

  it("drops a self-supersede (a data bug, never a valid write)", () => {
    const points = [{ id: "a", supersedes: "a" }];
    expect(supersedeEdges(points)).toEqual([]);
  });

  it("drops a null supersede", () => {
    const points = [{ id: "a", supersedes: null as unknown as undefined }];
    expect(supersedeEdges(points)).toEqual([]);
  });
});

describe("mergeAtlasEdges", () => {
  it("returns the base array unchanged (same reference) when extra is empty", () => {
    const base = [{ src: "a", dst: "b" }];
    expect(mergeAtlasEdges(base, [])).toBe(base);
  });

  it("concatenates without deduping — a link edge and a supersede edge are distinct facts", () => {
    const base = [{ src: "a", dst: "b" }];
    const extra = [{ src: "a", dst: "b" }, { src: "c", dst: "d" }];
    expect(mergeAtlasEdges(base, extra)).toEqual([
      { src: "a", dst: "b" },
      { src: "a", dst: "b" },
      { src: "c", dst: "d" },
    ]);
  });
});
