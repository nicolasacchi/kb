// MI-W4.3 — the lineage viewer is DELIBERATELY small: 2-6 nodes. The
// server already bounds a supersede-chain walk at `LINEAGE_MAX_HOPS` (200,
// a corruption/cycle guard, not a display budget) — this is the SEPARATE,
// much tighter display-side cap: paginate/truncate with a count rather
// than ever growing the viewer toward a node-link hairball (the design
// brief's one hard "never" for graph rendering).

export interface LineageDisplayPlan<T> {
  /** Closest-to-start first, capped at `maxPerSide`. */
  shownOlder: T[];
  olderTruncated: number;
  /** Closest-to-start first, capped at `maxPerSide`. */
  shownNewer: T[];
  newerTruncated: number;
}

export const DEFAULT_MAX_PER_SIDE = 2;

/**
 * `supersedesChain` (what `start` supersedes, closest-first) and
 * `supersededByChain` (what superseded `start`, closest-first) are the
 * lineage API's own two arrays, unchanged — this only decides how much of
 * each to actually RENDER.
 */
export function planLineageDisplay<T>(
  supersedesChain: T[],
  supersededByChain: T[],
  maxPerSide = DEFAULT_MAX_PER_SIDE,
): LineageDisplayPlan<T> {
  return {
    shownOlder: supersedesChain.slice(0, maxPerSide),
    olderTruncated: Math.max(0, supersedesChain.length - maxPerSide),
    shownNewer: supersededByChain.slice(0, maxPerSide),
    newerTruncated: Math.max(0, supersededByChain.length - maxPerSide),
  };
}
