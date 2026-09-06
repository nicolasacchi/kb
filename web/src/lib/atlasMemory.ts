// MI-W4.5 — pure helpers for the atlas's memory mode: turning a memory-scoped
// kb's `AtlasPoint`s (salience/decay_bucket/pinned/forgotten/supersedes,
// server-populated ONLY when `ctx.memory_scope` is set — see
// `routes/atlas.rs`'s `AtlasPoint` doc comment) into dot colors and supersede
// edges. No React, no fetch — colocated `atlasMemory.test.ts` covers it with
// plain vitest, same idiom as `atlasCameras.ts`/`atlasFit.ts`.

import { heatColor, heatT } from "../api/atlasField";
import type { AtlasEdge, AtlasPoint } from "../api/client";

/// The neutral grey a point paints when the metric a color mode wants is
/// absent (no salience, no decay bucket) — matches the field-disagreement
/// overlay's own "nothing to show" grey (`atlasField.ts`'s `heatOn` branch),
/// so an unmeasured dot always reads as "no data", never a wrong color.
export const MEMORY_COLOR_FALLBACK = "#666";

/// Does at least one point carry ANY memory metadata? Gates whether the SPA
/// even OFFERS the salience/decay color modes — a non-memory-scoped kb's
/// points never carry these fields (the server-side gate is the source of
/// truth; this is just "should the toggle exist at all").
export function hasMemoryPoints(
  points: Pick<AtlasPoint, "salience" | "decay_bucket" | "pinned" | "forgotten" | "supersedes">[],
): boolean {
  return points.some(
    (p) =>
      p.salience != null ||
      p.decay_bucket != null ||
      p.pinned != null ||
      p.forgotten != null ||
      p.supersedes != null,
  );
}

/// Salience (0..1) → a color on the same cool→hot ramp the field-
/// disagreement heat mode uses (`heatT`/`heatColor`) — high salience reads
/// hot, low salience reads cool, absent/invalid salience is the neutral
/// fallback. Reuses the existing ramp rather than inventing a second one.
export function salienceFillColor(salience: number | null | undefined): string {
  if (salience == null || !Number.isFinite(salience)) return MEMORY_COLOR_FALLBACK;
  return heatColor(heatT(salience, 1));
}

const DECAY_BUCKET_COLORS: Readonly<Record<string, string>> = {
  slow: "#5b8def",
  fast: "#e2564a",
};

/// The raw `kb-decay` bucket ("slow" | "fast") → a fixed two-color mapping
/// (an unrecognised or absent bucket is the neutral fallback, never a guess).
export function decayBucketFillColor(bucket: string | null | undefined): string {
  if (!bucket) return MEMORY_COLOR_FALLBACK;
  return DECAY_BUCKET_COLORS[bucket] ?? MEMORY_COLOR_FALLBACK;
}

/// One `AtlasEdge` per point that supersedes another point ALSO present in
/// `points` (a supersede target outside the current point set — paginated
/// away, or in a different kb — is silently dropped, same "never a dangling
/// edge" posture `edgeLines` already takes for the `kind=link` graph).
/// Self-supersede (a data bug, never a valid write) is dropped too. Reuses
/// the plain `{src, dst}` `AtlasEdge` shape so the caller feeds the result
/// straight into the SAME curved-edge draw path as the link graph — no new
/// visual machinery.
export function supersedeEdges(
  points: Pick<AtlasPoint, "id" | "supersedes">[],
): AtlasEdge[] {
  const ids = new Set(points.map((p) => p.id));
  const out: AtlasEdge[] = [];
  for (const p of points) {
    if (p.supersedes && p.supersedes !== p.id && ids.has(p.supersedes)) {
      out.push({ src: p.id, dst: p.supersedes });
    }
  }
  return out;
}

/// Concatenate the link-graph edges with the supersede edges for the shared
/// curved-edge draw path. A plain concat (not a Set-dedup) is correct here:
/// `edgeLines`'s own `seen` set already collapses directionless duplicates
/// at draw time, and a link edge and a supersede edge between the same two
/// ids are two DIFFERENT facts that happen to share endpoints — collapsing
/// them here would silently drop one.
export function mergeAtlasEdges(base: AtlasEdge[], extra: AtlasEdge[]): AtlasEdge[] {
  return extra.length === 0 ? base : [...base, ...extra];
}
