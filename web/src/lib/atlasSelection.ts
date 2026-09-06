// W2.3b — pure selection + layout math for the atlas view: freehand-lasso
// point-in-polygon, cluster→continent merging for the semantic-zoom label
// LOD, and the layout-stress comparison between a doc's true (embedding-
// space) neighbors and their 2-D projected placement.
//
// No React, no fetch, no DOM — colocated vitest (atlasSelection.test.ts),
// same shape as atlasCameras.ts / galleryUrl.ts. AtlasView.tsx and
// AtlasInspector.tsx are the only callers.

export type Vec2 = { x: number; y: number };

function dist2(a: Vec2, b: Vec2): number {
  const dx = a.x - b.x;
  const dy = a.y - b.y;
  return dx * dx + dy * dy;
}

// --- lasso: point-in-polygon + selection set --------------------------

/** Ray-casting point-in-polygon (even-odd rule) in the SAME logical (W×H)
 * coordinate space AtlasView already places dots in — callers pass
 * `screenToLogical`-derived points, never screen/CSS pixels. A polygon
 * under 3 points can't enclose anything, so a click-without-drag "lasso"
 * is a no-op selection rather than a thrown error. */
export function pointInPolygon(pt: Vec2, polygon: readonly Vec2[]): boolean {
  if (polygon.length < 3) return false;
  let inside = false;
  for (let i = 0, j = polygon.length - 1; i < polygon.length; j = i++) {
    const xi = polygon[i].x;
    const yi = polygon[i].y;
    const xj = polygon[j].x;
    const yj = polygon[j].y;
    const crosses = yi > pt.y !== yj > pt.y;
    if (crosses) {
      const xIntersect = ((xj - xi) * (pt.y - yi)) / (yj - yi) + xi;
      if (pt.x < xIntersect) inside = !inside;
    }
  }
  return inside;
}

/** Every point (by id) whose (x, y) falls inside the freehand polygon.
 * Returns an empty Set for a degenerate (<3-point) polygon instead of
 * throwing — the same "no-op, not an error" stance as `pointInPolygon`. */
export function selectionFromLasso(
  points: readonly { id: string; x: number; y: number }[],
  polygon: readonly Vec2[],
): Set<string> {
  const out = new Set<string>();
  if (polygon.length < 3) return out;
  for (const p of points) {
    if (pointInPolygon({ x: p.x, y: p.y }, polygon)) out.add(p.id);
  }
  return out;
}

// --- semantic zoom: cluster → continent merge --------------------------

/** Semantic-zoom "continent" level caps at 6 groups — see AtlasView's
 * ZOOM_LOD_CONTINENT threshold. The source is always ≤12 clusters
 * (server `MAX_CLUSTERS`), so the O(n³) agglomerative pass below is
 * trivial at this scale. */
export const MAX_CONTINENTS = 6;

export type TermFreq = { term: string; tf: number };

export type ClusterForMerge = {
  cluster: number;
  centroid: Vec2;
  count: number;
  /** Min y among the cluster's points (AtlasView's `topY`) — carried
   * through merges as the min over members, so a continent label sits
   * above its topmost dot the same way a per-cluster label does. */
  topY: number;
  /** Fallback label when no server term data exists for this cluster
   * (AtlasView's dominant-tag heuristic). */
  label: string;
  /** Server c-TF-IDF top terms for this cluster, already ranked; empty
   * when unavailable (this kb hasn't recomputed since W1.B, or the
   * cluster fell back to the tag heuristic). */
  terms: readonly TermFreq[];
};

export type ContinentGroup = {
  /** Source cluster ids, ascending. */
  members: number[];
  centroid: Vec2;
  count: number;
  topY: number;
  label: string;
};

/** Re-rank the union of each member's top terms by SUMMED term frequency.
 * `tf` adds validly across clusters (it's a raw count); `ft` — how many
 * OTHER clusters a term also appears in — does NOT get a merged
 * recomputation here: it was defined relative to the ORIGINAL per-cluster
 * partition, so once clusters are merged its "distinctiveness" meaning is
 * stale. Continent labels are therefore ranked by tf alone, not the
 * `tf * ln(1+A/ft)` score the per-cluster legend shows — an intentional,
 * honest downgrade rather than a fabricated merged score. */
function rerankTerms(termLists: readonly (readonly TermFreq[])[]): TermFreq[] {
  const sums = new Map<string, number>();
  for (const terms of termLists) {
    for (const t of terms) sums.set(t.term, (sums.get(t.term) ?? 0) + t.tf);
  }
  return [...sums.entries()]
    .map(([term, tf]) => ({ term, tf }))
    .sort((a, b) => b.tf - a.tf || (a.term < b.term ? -1 : a.term > b.term ? 1 : 0));
}

/** Label a continent from its merged terms when any member had server
 * terms (top-2, mirroring the per-cluster legend's `serverTerms.slice(0,
 * 2)`); else fall back to the member clusters' own labels (dominant
 * tags), largest-membership-first, so a term-less continent still reads
 * as something rather than "cluster 0 + cluster 3". */
function labelContinent(members: readonly ClusterForMerge[]): string {
  const anyTerms = members.some((m) => m.terms.length > 0);
  if (anyTerms) {
    const ranked = rerankTerms(members.map((m) => m.terms));
    if (ranked.length > 0) {
      return ranked.slice(0, 2).map((t) => t.term).join(" · ");
    }
  }
  const byCount = [...members].sort(
    (a, b) => b.count - a.count || a.cluster - b.cluster,
  );
  const labels = [...new Set(byCount.map((m) => m.label))];
  return labels.slice(0, 2).join(" + ");
}

/** Agglomerative merge of ≤12 cluster centroids down to `maxContinents`
 * groups (default 6) — the semantic-zoom "continent" level. Deterministic:
 * repeatedly merges the closest pair of GROUP centroids (weighted by
 * point count so a big cluster pulls the merged centroid toward it); ties
 * break on the lowest (i, j) pair found by a strict `<` scan over the
 * current, stably-ordered group array (groups start sorted by ascending
 * cluster id and only change order by append-on-merge), never on
 * iteration-order happenstance. A no-op (clusters.length ≤ maxContinents)
 * returns one group per cluster, sorted the same way, so the continent
 * view degrades to the cluster view exactly when there's nothing to
 * merge. */
export function mergeContinents(
  clusters: readonly ClusterForMerge[],
  maxContinents: number = MAX_CONTINENTS,
): ContinentGroup[] {
  type WorkGroup = {
    members: ClusterForMerge[];
    centroid: Vec2;
    count: number;
    topY: number;
  };
  const groups: WorkGroup[] = clusters
    .slice()
    .sort((a, b) => a.cluster - b.cluster)
    .map((c) => ({
      members: [c],
      centroid: c.centroid,
      count: c.count,
      topY: c.topY,
    }));

  while (groups.length > maxContinents && groups.length > 1) {
    let bi = 0;
    let bj = 1;
    let best = Infinity;
    for (let i = 0; i < groups.length; i++) {
      for (let j = i + 1; j < groups.length; j++) {
        const d = dist2(groups[i].centroid, groups[j].centroid);
        if (d < best) {
          best = d;
          bi = i;
          bj = j;
        }
      }
    }
    const a = groups[bi];
    const b = groups[bj];
    const count = a.count + b.count;
    const merged: WorkGroup = {
      members: [...a.members, ...b.members],
      centroid: {
        x: (a.centroid.x * a.count + b.centroid.x * b.count) / count,
        y: (a.centroid.y * a.count + b.centroid.y * b.count) / count,
      },
      count,
      topY: Math.min(a.topY, b.topY),
    };
    // Remove the higher index first so the lower index's splice doesn't
    // shift it out from under `bi`.
    groups.splice(bj, 1);
    groups.splice(bi, 1);
    groups.push(merged);
  }

  return groups
    .map((g) => ({
      members: g.members.map((m) => m.cluster).sort((a, b) => a - b),
      centroid: g.centroid,
      count: g.count,
      topY: g.topY,
      label: labelContinent(g.members),
    }))
    .sort((a, b) => a.members[0] - b.members[0]);
}

// --- layout stress: high-D neighbors vs. their 2-D placement -----------

export type StressPoint = { id: string; x: number; y: number };

/** The k nearest OTHER points to `id` by Euclidean distance in the given
 * 2-D coordinate space (AtlasView's logical atlas coords). Ties break on
 * ascending id so the result is reproducible even when several points
 * share identical (fallback-hashed) coordinates. */
export function knn2d(
  id: string,
  points: readonly StressPoint[],
  k: number,
): Set<string> {
  const self = points.find((p) => p.id === id);
  if (!self || k <= 0) return new Set();
  const ranked = points
    .filter((p) => p.id !== id)
    .map((p) => ({ id: p.id, d2: dist2(self, p) }))
    .sort((a, b) => a.d2 - b.d2 || (a.id < b.id ? -1 : a.id > b.id ? 1 : 0));
  return new Set(ranked.slice(0, k).map((r) => r.id));
}

export type LayoutStress = { farCount: number; total: number };

/** Distill's "the map lies" lesson (dimensionality-reduction plots always
 * distort SOME distances — see distill.pub's t-SNE/UMAP pieces), made
 * concrete per selection: of `highDNeighborIds` (the true, embedding-space
 * nearest neighbors, already ranked by the server), how many are NOT among
 * that same count's nearest points in the 2-D projection? A high
 * `farCount` says the projection compresses distances the embedding
 * doesn't — an honest caveat on the constellation, not a defect in it.
 * `total === 0` (no true-neighbor data yet) is the caller's cue to fall
 * back to the old "neighbors land in a follow-up" marker text. */
export function layoutStress(
  id: string,
  highDNeighborIds: readonly string[],
  points: readonly StressPoint[],
): LayoutStress {
  const k = highDNeighborIds.length;
  if (k === 0) return { farCount: 0, total: 0 };
  const near2d = knn2d(id, points, k);
  let farCount = 0;
  for (const nid of highDNeighborIds) {
    if (!near2d.has(nid)) farCount += 1;
  }
  return { farCount, total: k };
}
