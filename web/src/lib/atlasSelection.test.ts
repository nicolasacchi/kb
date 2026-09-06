import { describe, expect, it } from "vitest";
import {
  knn2d,
  layoutStress,
  mergeContinents,
  pointInPolygon,
  selectionFromLasso,
  type ClusterForMerge,
} from "./atlasSelection";

describe("pointInPolygon", () => {
  const square = [
    { x: 0, y: 0 },
    { x: 10, y: 0 },
    { x: 10, y: 10 },
    { x: 0, y: 10 },
  ];

  it("finds a center point inside a square", () => {
    expect(pointInPolygon({ x: 5, y: 5 }, square)).toBe(true);
  });

  it("excludes a point clearly outside", () => {
    expect(pointInPolygon({ x: 20, y: 20 }, square)).toBe(false);
    expect(pointInPolygon({ x: -5, y: 5 }, square)).toBe(false);
  });

  it("handles a concave (arrow-shaped) polygon", () => {
    const arrow = [
      { x: 0, y: 0 },
      { x: 10, y: 0 },
      { x: 5, y: 5 },
      { x: 10, y: 10 },
      { x: 0, y: 10 },
    ];
    // The notch cut into the right edge — a point sitting in the notch
    // is outside the shape even though it's within the bounding box.
    expect(pointInPolygon({ x: 8, y: 5 }, arrow)).toBe(false);
    expect(pointInPolygon({ x: 2, y: 5 }, arrow)).toBe(true);
  });

  it("degenerate polygons (<3 points) never enclose anything", () => {
    expect(pointInPolygon({ x: 0, y: 0 }, [])).toBe(false);
    expect(pointInPolygon({ x: 0, y: 0 }, [{ x: 0, y: 0 }])).toBe(false);
    expect(
      pointInPolygon({ x: 0, y: 0 }, [
        { x: 0, y: 0 },
        { x: 1, y: 1 },
      ]),
    ).toBe(false);
  });
});

describe("selectionFromLasso", () => {
  const square = [
    { x: 0, y: 0 },
    { x: 10, y: 0 },
    { x: 10, y: 10 },
    { x: 0, y: 10 },
  ];

  it("returns the ids of every enclosed point", () => {
    const points = [
      { id: "in-1", x: 2, y: 2 },
      { id: "in-2", x: 8, y: 8 },
      { id: "out", x: 50, y: 50 },
    ];
    expect(selectionFromLasso(points, square)).toEqual(
      new Set(["in-1", "in-2"]),
    );
  });

  it("is empty for a degenerate polygon", () => {
    const points = [{ id: "a", x: 1, y: 1 }];
    expect(selectionFromLasso(points, [{ x: 0, y: 0 }])).toEqual(new Set());
  });
});

describe("mergeContinents", () => {
  function cluster(
    id: number,
    x: number,
    y: number,
    count: number,
    topY: number,
    label: string,
    terms: { term: string; tf: number }[] = [],
  ): ClusterForMerge {
    return { cluster: id, centroid: { x, y }, count, topY, label, terms };
  }

  it("is a no-op when already at or under the cap", () => {
    const clusters = [cluster(0, 0, 0, 3, -5, "alpha"), cluster(1, 100, 0, 2, -2, "beta")];
    const groups = mergeContinents(clusters, 6);
    expect(groups).toEqual([
      { members: [0], centroid: { x: 0, y: 0 }, count: 3, topY: -5, label: "alpha" },
      { members: [1], centroid: { x: 100, y: 0 }, count: 2, topY: -2, label: "beta" },
    ]);
  });

  it("merges two close pairs into two continents, tie-broken deterministically", () => {
    // Two symmetric close pairs — (0,1) and (2,3) both have centroid
    // distance 1, and (0,1) is scanned first (lowest i,j), so it merges
    // first regardless of tie.
    const clusters = [
      cluster(0, 0, 0, 3, -1, "a"),
      cluster(1, 1, 0, 2, -2, "b"),
      cluster(2, 100, 100, 4, 90, "c"),
      cluster(3, 101, 100, 1, 95, "d"),
    ];
    const groups = mergeContinents(clusters, 2);
    expect(groups).toHaveLength(2);
    const [g0, g1] = groups;
    expect(g0.members).toEqual([0, 1]);
    expect(g0.count).toBe(5);
    expect(g0.centroid.x).toBeCloseTo(0.4, 10);
    expect(g0.centroid.y).toBeCloseTo(0, 10);
    expect(g0.topY).toBe(-2); // min(-1, -2)
    expect(g1.members).toEqual([2, 3]);
    expect(g1.count).toBe(5);
    expect(g1.centroid.x).toBeCloseTo(100.2, 10);
    expect(g1.topY).toBe(90);
  });

  it("labels from summed term frequency when server terms exist", () => {
    const clusters = [
      cluster(0, 0, 0, 3, -1, "tag-a", [
        { term: "rust", tf: 5 },
        { term: "async", tf: 2 },
      ]),
      cluster(1, 1, 0, 2, -2, "tag-b", [
        { term: "rust", tf: 1 },
        { term: "tokio", tf: 4 },
      ]),
    ];
    const [group] = mergeContinents(clusters, 1);
    // rust: 5+1=6, tokio: 4, async: 2 — top 2 by summed tf.
    expect(group.label).toBe("rust · tokio");
  });

  it("falls back to member labels (by count, tie on cluster id) when no terms exist anywhere", () => {
    const clusters = [
      cluster(0, 0, 0, 2, -1, "alpha"),
      cluster(1, 1, 0, 9, -2, "beta"),
      cluster(2, 2, 0, 9, -3, "gamma"),
    ];
    const [group] = mergeContinents(clusters, 1);
    // beta and gamma tie on count (9); cluster id 1 < 2 wins the tie.
    expect(group.label).toBe("beta + gamma");
  });
});

describe("knn2d / layoutStress", () => {
  const points = [
    { id: "self", x: 0, y: 0 },
    { id: "near-1", x: 1, y: 0 },
    { id: "near-2", x: 0, y: 1 },
    { id: "far-1", x: 100, y: 100 },
    { id: "far-2", x: 200, y: 200 },
  ];

  it("finds the k nearest other points, excluding self", () => {
    expect(knn2d("self", points, 2)).toEqual(new Set(["near-1", "near-2"]));
  });

  it("returns empty for an unknown id or k<=0", () => {
    expect(knn2d("missing", points, 2)).toEqual(new Set());
    expect(knn2d("self", points, 0)).toEqual(new Set());
  });

  it("layoutStress is 0/0 with no true-neighbor data (the fallback signal)", () => {
    expect(layoutStress("self", [], points)).toEqual({ farCount: 0, total: 0 });
  });

  it("counts true neighbors that land far in the 2-D projection", () => {
    // True (embedding-space) neighbors say near-1 and far-1 are closest —
    // but far-1 is nowhere near "self" in the 2-D layout (near-2 is
    // closer), so it counts as one "far" mismatch out of two.
    const stress = layoutStress("self", ["near-1", "far-1"], points);
    expect(stress).toEqual({ farCount: 1, total: 2 });
  });

  it("is fully consistent (farCount 0) when high-D and 2-D agree", () => {
    const stress = layoutStress("self", ["near-1", "near-2"], points);
    expect(stress).toEqual({ farCount: 0, total: 2 });
  });
});
