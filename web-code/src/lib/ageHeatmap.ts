// Pure age-bucket computation for the reader's "age" provenance overlay
// (Wave C — the third state of the Provenance toggle, alongside `off`/
// `dots`). Buckets every already-fetched `GET /api/blame` region's own
// `author_time` into `AGE_BUCKET_COUNT` buckets spanning this FILE's own
// [oldest, newest] region time — RELATIVE to the file, not wall-clock
// "now": a file whose every line was last touched two years ago should
// still show its own newest/oldest gradient (the lines edited most
// recently relative to each other are still the "warmest" in THIS file),
// not read uniformly cold just because the whole file is old. `editor/
// ageOverlay.ts` is the CM6-facing consumer (a line-background Decoration
// set keyed on this module's output) — kept DOM-free here, same split as
// `lib/blameGutter.ts` vs. `editor/lineGutter.ts`.
//
// Churn heatmap (per-line historical edit VOLUME — how many times a line
// has changed, not just how recently) is OUT OF SCOPE here: it would need a
// `git log -L <line>,<line>:<path>` walk per line/region, an order of
// magnitude more expensive than the single `GET /api/blame` this overlay
// already has in hand — leave it to a future wave that wants to pay that
// cost.

import type { BlameRegion } from "../api/types";

/// 0 = newest/warmest, `AGE_BUCKET_COUNT - 1` = oldest/coldest.
export const AGE_BUCKET_COUNT = 5;
export type AgeBucket = 0 | 1 | 2 | 3 | 4;

export interface AgeLineInfo {
  bucket: AgeBucket;
  author_time: number;
}

/// Bucket every region's `author_time` into `AGE_BUCKET_COUNT` equal-width
/// buckets spanning [oldest, newest] region time IN THIS FILE (see the
/// module doc). A file with only ONE distinct `author_time` across every
/// region (every line from one commit — a fresh file) buckets everything to
/// `0` (warmest) rather than dividing by a zero span.
export function buildAgeLineBuckets(regions: BlameRegion[]): Map<number, AgeLineInfo> {
  const out = new Map<number, AgeLineInfo>();
  if (regions.length === 0) return out;

  let min = Infinity;
  let max = -Infinity;
  for (const r of regions) {
    if (r.author_time < min) min = r.author_time;
    if (r.author_time > max) max = r.author_time;
  }
  const span = max - min;

  for (const region of regions) {
    const bucket: AgeBucket =
      span <= 0
        ? 0
        : (Math.min(
            AGE_BUCKET_COUNT - 1,
            Math.floor(((max - region.author_time) / span) * AGE_BUCKET_COUNT),
          ) as AgeBucket);
    const end = region.final_start + region.count;
    for (let line = region.final_start; line < end; line++) {
      out.set(line, { bucket, author_time: region.author_time });
    }
  }
  return out;
}
