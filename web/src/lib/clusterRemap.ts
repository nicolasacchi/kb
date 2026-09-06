// W3.T-c — cluster-id remapping for the atlas time-lapse. Pure, no
// DOM/React, colocated vitest (clusterRemap.test.ts) — same shape as
// atlasFit.ts / atlasSelection.ts / atlasCameras.ts.
//
// WHY THIS EXISTS (the load-bearing correctness piece of the time-lapse):
// `kmeans_lloyd` (crates/kb-core/src/atlas.rs ~:404-490) seeds its
// centroids by ARRAY POSITION and reseeds an emptied cluster randomly, and
// the GC-B1 note at ~:634-645 records the consequence — any reorder of the
// input renumbers the clusters. So cluster id 3 in frame A and cluster id 3
// in frame B are UNRELATED labels, not the same region of the map. Playing
// a time-lapse straight off the stored ids therefore makes the palette
// strobe: a continent that never moved flips colour every frame for no
// reason the operator can see, and the strobe reads as "the corpus
// reorganised itself" when nothing happened at all.
//
// The fix is a deterministic greedy nearest-centroid match of each frame's
// cluster ids onto the NEWEST frame's, computed on 2-D centroids, ties
// broken by id (never by input order). It is applied to COLOUR ONLY:
// AtlasView keeps every dot's own `cluster` — the tooltip, the legend
// grouping, the highlight filter — exactly as the daemon recorded it, and
// paints with a separate `colorCluster`. A remap is a *display* convenience,
// NOT a claim that two clusterings are the same partition: nothing here
// merges, splits, or rewrites stored cluster data, and a frame cluster with
// no counterpart in the target keeps a colour of its own rather than being
// folded into somebody else's.
//
// Deliberately NOT a Hungarian/optimal assignment: greedy-nearest is what
// the SPA can state in one sentence to the operator, and an optimal
// matching would be a stronger claim about correspondence than centroid
// proximity across two independent k-means runs can support.

/** A cluster's 2-D centroid in whatever coordinate space the caller is
 * working in (AtlasView passes logical W×H space, so frame points and the
 * live map are directly comparable — both go through the same affine
 * unit→logical placement first). */
export type ClusterCentroid = { cluster: number; x: number; y: number };

/** One placed point: its cluster id plus its 2-D position. */
export type ClusterPoint = { cluster: number; x: number; y: number };

function finite(n: number): boolean {
  return Number.isFinite(n);
}

/** Lexicographic (cluster, x, y) — the canonical order everything below
 * sorts into, so no result ever depends on the caller's array order
 * (including the floating-point SUM order inside `centroidsOf`, which
 * would otherwise differ in the last bit under a reordering). */
function byClusterThenXY(a: ClusterPoint, b: ClusterPoint): number {
  return a.cluster - b.cluster || a.x - b.x || a.y - b.y;
}

/** Mean position per cluster, ascending by cluster id. Points with a
 * non-finite coordinate are skipped (a cluster with no finite points is
 * omitted entirely rather than emitting a NaN centroid that would poison
 * every distance comparison downstream). */
export function centroidsOf(points: readonly ClusterPoint[]): ClusterCentroid[] {
  const clean = points.filter((p) => finite(p.x) && finite(p.y));
  clean.sort(byClusterThenXY);
  const out: ClusterCentroid[] = [];
  let i = 0;
  while (i < clean.length) {
    const cluster = clean[i].cluster;
    let sx = 0;
    let sy = 0;
    let n = 0;
    while (i < clean.length && clean[i].cluster === cluster) {
      sx += clean[i].x;
      sy += clean[i].y;
      n += 1;
      i += 1;
    }
    out.push({ cluster, x: sx / n, y: sy / n });
  }
  return out;
}

/** Collapse to one centroid per cluster id, ascending. Duplicate ids
 * shouldn't happen (`centroidsOf` never emits them) — when they do, the
 * lexicographically smallest (x, y) wins, which is order-independent
 * (averaging would not be, at the last floating-point bit). */
function canonicalise(cs: readonly ClusterCentroid[]): ClusterCentroid[] {
  const best = new Map<number, ClusterCentroid>();
  for (const c of cs) {
    if (!finite(c.x) || !finite(c.y)) continue;
    const cur = best.get(c.cluster);
    if (!cur || c.x < cur.x || (c.x === cur.x && c.y < cur.y)) {
      best.set(c.cluster, { cluster: c.cluster, x: c.x, y: c.y });
    }
  }
  return [...best.values()].sort((a, b) => a.cluster - b.cluster);
}

/**
 * Greedy nearest-centroid match of `frame`'s cluster ids onto `target`'s.
 *
 * Every pair (frame cluster, target cluster) is scored by squared centroid
 * distance and taken in ascending distance order, ties broken by frame id
 * then target id; a pair is accepted when neither side is already spoken
 * for. Frame clusters left over (the frame has MORE clusters than the
 * target, or its extra centroids are non-finite) are assigned fresh ids
 * above every id in play, ascending — so they get a colour of their own
 * instead of silently borrowing a matched cluster's.
 *
 * Returns frame-cluster-id → colour-cluster-id. Ids absent from the map
 * (nothing to match against) should fall back to themselves — see
 * [`remappedCluster`].
 */
export function remapClusters(
  frame: readonly ClusterCentroid[],
  target: readonly ClusterCentroid[],
): Map<number, number> {
  const f = canonicalise(frame);
  const t = canonicalise(target);
  const out = new Map<number, number>();

  const pairs: { fi: number; ti: number; d: number }[] = [];
  for (const a of f) {
    for (const b of t) {
      const dx = a.x - b.x;
      const dy = a.y - b.y;
      pairs.push({ fi: a.cluster, ti: b.cluster, d: dx * dx + dy * dy });
    }
  }
  // Distance first, then BOTH ids — a total order, so the greedy walk is a
  // pure function of the two centroid sets and never of their array order.
  pairs.sort((p, q) => p.d - q.d || p.fi - q.fi || p.ti - q.ti);

  const usedFrom = new Set<number>();
  const usedTo = new Set<number>();
  for (const p of pairs) {
    if (usedFrom.has(p.fi) || usedTo.has(p.ti)) continue;
    usedFrom.add(p.fi);
    usedTo.add(p.ti);
    out.set(p.fi, p.ti);
  }

  // Leftovers: a fresh slot each, above every target id and every id
  // already handed out, ascending by frame cluster id.
  let next = -1;
  for (const b of t) if (b.cluster > next) next = b.cluster;
  for (const v of out.values()) if (v > next) next = v;
  next += 1;
  for (const a of f) {
    if (out.has(a.cluster)) continue;
    out.set(a.cluster, next);
    next += 1;
  }
  return out;
}

/** Apply a remap, falling back to the cluster's own id when it isn't in
 * the map (nothing to match against — e.g. the target frame had no
 * clusters at all). */
export function remappedCluster(
  remap: ReadonlyMap<number, number> | null | undefined,
  cluster: number,
): number {
  if (!remap) return cluster;
  return remap.get(cluster) ?? cluster;
}
