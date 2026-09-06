import { describe, expect, it } from "vitest";
import {
  centroidsOf,
  remapClusters,
  remappedCluster,
  type ClusterCentroid,
  type ClusterPoint,
} from "./clusterRemap";

/** Map → sorted [from, to] pairs, so an assertion never depends on
 * insertion order. */
const entries = (m: ReadonlyMap<number, number>) =>
  [...m.entries()].sort((a, b) => a[0] - b[0]);

/** Deterministic shuffle (no Math.random in a test that pins
 * determinism): a fixed rotation + reversal is enough to break any
 * accidental reliance on the caller's array order. */
function reorder<T>(xs: readonly T[]): T[] {
  const out = [...xs];
  out.reverse();
  const head = out.shift();
  if (head !== undefined) out.splice(Math.floor(out.length / 2), 0, head);
  return out;
}

describe("centroidsOf", () => {
  it("means each cluster and sorts ascending by cluster id", () => {
    const pts: ClusterPoint[] = [
      { cluster: 2, x: 10, y: 0 },
      { cluster: 0, x: 0, y: 0 },
      { cluster: 0, x: 2, y: 4 },
      { cluster: 2, x: 12, y: 2 },
    ];
    expect(centroidsOf(pts)).toEqual([
      { cluster: 0, x: 1, y: 2 },
      { cluster: 2, x: 11, y: 1 },
    ]);
  });

  it("skips non-finite coords and drops a cluster with none left", () => {
    const pts: ClusterPoint[] = [
      { cluster: 0, x: NaN, y: 0 },
      { cluster: 1, x: 4, y: 4 },
      { cluster: 1, x: Infinity, y: 0 },
    ];
    expect(centroidsOf(pts)).toEqual([{ cluster: 1, x: 4, y: 4 }]);
  });

  it("is bit-identical under input reordering", () => {
    const pts: ClusterPoint[] = [
      { cluster: 0, x: 0.1, y: 0.7 },
      { cluster: 0, x: 0.2, y: 0.3 },
      { cluster: 0, x: 0.30000000000000004, y: 0.1 },
      { cluster: 1, x: 9.9, y: 1.1 },
      { cluster: 1, x: 8.8, y: 2.2 },
    ];
    expect(centroidsOf(reorder(pts))).toEqual(centroidsOf(pts));
  });
});

describe("remapClusters", () => {
  // Four well-separated centroids — the "continents" a frame and the
  // newest frame would share when nothing actually moved.
  const A: ClusterCentroid[] = [
    { cluster: 0, x: 0, y: 0 },
    { cluster: 1, x: 100, y: 0 },
    { cluster: 2, x: 0, y: 100 },
    { cluster: 3, x: 100, y: 100 },
  ];

  it("identity — a frame aligned against itself keeps every id", () => {
    expect(entries(remapClusters(A, A))).toEqual([
      [0, 0],
      [1, 1],
      [2, 2],
      [3, 3],
    ]);
  });

  it("permuted — the same geometry with renumbered ids is recovered", () => {
    // kmeans_lloyd seeds by array position, so the SAME four regions come
    // back under different labels on the next recompute. Positions are
    // nudged so this isn't a zero-distance degenerate case.
    const permuted: ClusterCentroid[] = [
      { cluster: 0, x: 101, y: 99 }, // was A#3
      { cluster: 1, x: 1, y: 1 }, // was A#0
      { cluster: 2, x: 99, y: 2 }, // was A#1
      { cluster: 3, x: 2, y: 101 }, // was A#2
    ];
    expect(entries(remapClusters(permuted, A))).toEqual([
      [0, 3],
      [1, 0],
      [2, 1],
      [3, 2],
    ]);
  });

  it("fewer clusters than the target — each maps to its nearest, no leftovers", () => {
    const early: ClusterCentroid[] = [
      { cluster: 0, x: 2, y: 98 }, // nearest A#2
      { cluster: 1, x: 98, y: 3 }, // nearest A#1
    ];
    expect(entries(remapClusters(early, A))).toEqual([
      [0, 2],
      [1, 1],
    ]);
  });

  it("more clusters than the target — extras get fresh, non-colliding ids", () => {
    const target: ClusterCentroid[] = [
      { cluster: 0, x: 0, y: 0 },
      { cluster: 1, x: 100, y: 100 },
    ];
    const busy: ClusterCentroid[] = [
      { cluster: 0, x: 1, y: 1 }, // → target 0
      { cluster: 1, x: 99, y: 99 }, // → target 1
      { cluster: 2, x: 50, y: 50 }, // no counterpart
      { cluster: 3, x: 55, y: 45 }, // no counterpart
    ];
    const m = remapClusters(busy, target);
    expect(entries(m)).toEqual([
      [0, 0],
      [1, 1],
      [2, 2],
      [3, 3],
    ]);
    // The two leftovers must not reuse a colour already claimed by a
    // matched cluster — that would assert a correspondence that isn't there.
    const assigned = [...m.values()];
    expect(new Set(assigned).size).toBe(assigned.length);
    expect(assigned.filter((v) => v === 0 || v === 1)).toHaveLength(2);
  });

  it("leftovers clear the target's id space even when it is sparse", () => {
    const target: ClusterCentroid[] = [{ cluster: 7, x: 0, y: 0 }];
    const frame: ClusterCentroid[] = [
      { cluster: 0, x: 1, y: 1 },
      { cluster: 1, x: 90, y: 90 },
    ];
    expect(entries(remapClusters(frame, target))).toEqual([
      [0, 7],
      [1, 8],
    ]);
  });

  it("empty target — nothing to match, ids compact from 0 upward", () => {
    expect(entries(remapClusters(A, []))).toEqual([
      [0, 0],
      [1, 1],
      [2, 2],
      [3, 3],
    ]);
  });

  it("is deterministic under input reordering (both sides)", () => {
    const permuted: ClusterCentroid[] = [
      { cluster: 0, x: 101, y: 99 },
      { cluster: 1, x: 1, y: 1 },
      { cluster: 2, x: 99, y: 2 },
      { cluster: 3, x: 2, y: 101 },
      { cluster: 9, x: 50, y: 50 },
    ];
    const base = entries(remapClusters(permuted, A));
    expect(entries(remapClusters(reorder(permuted), A))).toEqual(base);
    expect(entries(remapClusters(permuted, reorder(A)))).toEqual(base);
    expect(entries(remapClusters(reorder(permuted), reorder(A)))).toEqual(base);
  });

  it("ties break by id, never by array order", () => {
    // Two frame clusters equidistant from one target cluster: the LOWER
    // frame id wins the match, the other becomes a leftover.
    const target: ClusterCentroid[] = [{ cluster: 0, x: 0, y: 0 }];
    const tied: ClusterCentroid[] = [
      { cluster: 5, x: 10, y: 0 },
      { cluster: 2, x: -10, y: 0 },
    ];
    expect(entries(remapClusters(tied, target))).toEqual([
      [2, 0],
      [5, 1],
    ]);
    expect(entries(remapClusters(reorder(tied), target))).toEqual([
      [2, 0],
      [5, 1],
    ]);
  });
});

describe("remappedCluster", () => {
  it("falls back to the cluster's own id", () => {
    expect(remappedCluster(null, 4)).toBe(4);
    expect(remappedCluster(new Map(), 4)).toBe(4);
    expect(remappedCluster(new Map([[4, 1]]), 4)).toBe(1);
  });
});
