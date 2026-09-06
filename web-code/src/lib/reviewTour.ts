// PRR-F — Guided review tour (design-ui.md §12.3, frontier 3). Pure fusion
// of reading-order (`GET /api/reviews/{id}/reading-order`, already fetched
// by `routes/ReviewDiff.tsx` for file ordering) + findings (`GET
// /api/reviews/{id}/findings`, already fetched for the overlay) into ONE
// flat, ordered stop list a keyboard flow walks — no new server route, no
// new fetch. State is client-only (`?tour=1` + a component-state index,
// per this unit's own brief) — nothing here is ever persisted.

import type { ReviewFinding } from "../api/types";
import { severityRank } from "./diffFindings";

export interface TourStop {
  path: string;
  /// `undefined` for a file with no findings — a plain "read this file"
  /// stop. Present for a finding-stop (one stop PER finding, severity-
  /// ordered within the file).
  findingSlug?: string;
  annotationId?: string;
}

/// One stop per reading-order file with no findings; one stop PER finding
/// (severity order — blocker, then concern, then ok; ties broken by
/// `created_at` ascending, the SAME deterministic tiebreak `sort_inbox_rows`
/// establishes elsewhere in this codebase) for a file WITH findings. A
/// file's own "just read it" stop is never emitted once at least one
/// finding-stop already visits that file — findings ARE the reason to stop
/// there.
export function buildTourStops(
  orderedPaths: readonly string[],
  findings: readonly ReviewFinding[],
): TourStop[] {
  const byPath = new Map<string, ReviewFinding[]>();
  for (const f of findings) {
    const list = byPath.get(f.location.path);
    if (list) list.push(f);
    else byPath.set(f.location.path, [f]);
  }
  for (const list of byPath.values()) {
    list.sort(
      (a, b) => severityRank(a.severity) - severityRank(b.severity) || a.created_at - b.created_at,
    );
  }
  const stops: TourStop[] = [];
  for (const path of orderedPaths) {
    const fs = byPath.get(path);
    if (!fs || fs.length === 0) {
      stops.push({ path });
      continue;
    }
    for (const f of fs) {
      stops.push({ path, findingSlug: f.slug, annotationId: f.annotation_id });
    }
  }
  return stops;
}

/// A stable per-stop identity for React keys / equality checks — `path`
/// alone for a file-only stop, `path#slug` for a finding stop (a slug is
/// unique per review, so this never collides across files).
export function tourStopKey(stop: TourStop): string {
  return stop.findingSlug ? `${stop.path}#${stop.findingSlug}` : stop.path;
}

/// Clamp a proposed tour index into `[0, stops.length - 1]`, or `null` when
/// `stops` is empty (nothing to land on) — the ONE place `advanceTour`'s
/// bounds check lives, so a caller never open-codes the clamp twice.
export function clampTourStep(idx: number, stopsLength: number): number | null {
  if (stopsLength === 0) return null;
  if (idx < 0) return 0;
  if (idx >= stopsLength) return stopsLength - 1;
  return idx;
}
