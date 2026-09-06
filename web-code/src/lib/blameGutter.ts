// Pure region→line-dot mapping for the blame gutter's disclosure ladder
// (W4.4, step 1: "a quiet dot on lines whose region resolves"). Kept free of
// CodeMirror/DOM concerns — `editor/lineGutter.ts` is the thin CM6 adapter
// that renders whatever this module computes; this module owns the actual
// decision logic so it's testable without a browser.

import type { AttributionOut, BlameRegion } from "../api/types";

export interface BlameDotInfo {
  /// trailer/exact confidence renders solid; fuzzy renders outlined (the
  /// `none` case never reaches here — see `buildLineDots`).
  solid: boolean;
  /// Hover-chip text: session display name (or the commit subject as a
  /// fallback) plus the confidence label.
  label: string;
  sha: string;
  regionStart: number;
  regionEnd: number;
}

/// The blame region covering `line` (1-based), or `undefined` if `line` is
/// out of range for `regions` (a stale `gotoLine`/gutter click racing a
/// file switch — the caller should treat this as "nothing to show", not an
/// error).
export function regionCoveringLine(regions: BlameRegion[], line: number): BlameRegion | undefined {
  return regions.find((r) => line >= r.final_start && line < r.final_start + r.count);
}

/// Every distinct sha across `regions`, in first-seen order — the set
/// `useBlameAttributions` lazily resolves one `/api/why?line=` call per
/// entry for (cached by sha, per the W4.4 brief: "fetch attribution lazily
/// per REGION … cached per sha").
export function distinctRegionShas(regions: BlameRegion[]): string[] {
  const seen = new Set<string>();
  const out: string[] = [];
  for (const r of regions) {
    if (!seen.has(r.sha)) {
      seen.add(r.sha);
      out.push(r.sha);
    }
  }
  return out;
}

function attributionLabel(attribution: AttributionOut, region: BlameRegion): string {
  const who = attribution.display_name ?? region.subject;
  return `${who} · ${attribution.confidence}`;
}

/// Fold `regions` + whatever attributions have resolved so far into a
/// per-line dot map. A region whose attribution hasn't loaded yet (not in
/// `attributionBySha`) or resolved to `confidence: "none"` gets NO dot on
/// any of its lines — "join confidence != none" is the gate, honest absence
/// otherwise (the gutter stays quiet; the panel's own honest-absence
/// copy is `WhyPanel`'s job, not this map's).
export function buildLineDots(
  regions: BlameRegion[],
  attributionBySha: Map<string, AttributionOut>,
): Map<number, BlameDotInfo> {
  const dots = new Map<number, BlameDotInfo>();
  for (const region of regions) {
    const attribution = attributionBySha.get(region.sha);
    if (!attribution || attribution.confidence === "none") continue;
    const info: BlameDotInfo = {
      solid: attribution.confidence === "trailer" || attribution.confidence === "exact",
      label: attributionLabel(attribution, region),
      sha: region.sha,
      regionStart: region.final_start,
      regionEnd: region.final_start + region.count - 1,
    };
    const end = region.final_start + region.count;
    for (let line = region.final_start; line < end; line++) {
      dots.set(line, info);
    }
  }
  return dots;
}
