// PRR-F — Guided review tour (design-ui.md §12.3, frontier 3). Pure fusion
// of reading-order (`GET /api/reviews/{id}/reading-order`, already fetched
// by `routes/ReviewDiff.tsx` for file ordering) + findings (`GET
// /api/reviews/{id}/findings`, already fetched for the overlay) into ONE
// flat, ordered stop list a keyboard flow walks — no new server route, no
// new fetch. State is client-only (`?tour=1` + a component-state index,
// per this unit's own brief) — nothing here is ever persisted.

import type { ReviewFinding } from "../api/types";
import { severityRank } from "./diffFindings";
import type { HumanOpenThread } from "./reviewRoom";

export interface TourStop {
  path: string;
  /// `undefined` for a file with no findings — a plain "read this file"
  /// stop. Present for a finding-stop (one stop PER finding, severity-
  /// ordered within the file).
  findingSlug?: string;
  annotationId?: string;
}

/// One stop per reading-order file with no findings/threads; one stop PER
/// finding (severity order — blocker, then concern, then ok; ties broken by
/// `created_at` ascending, the SAME deterministic tiebreak `sort_inbox_rows`
/// establishes elsewhere in this codebase), THEN one stop per open human
/// thread on that file (`created_at` ascending) that is not ALSO a
/// finding's own backing annotation (V80-M4: a manual finding's thread is
/// human-authored and would otherwise double-visit the same annotation —
/// `findingAnnotationIds` guards that). A file's own "just read it" stop is
/// never emitted once at least one finding-stop or thread-stop already
/// visits that file — findings and open human threads both ARE reasons to
/// stop there. `humanThreads` defaults to `[]` — every pre-M4 caller/test
/// is byte-identical.
export function buildTourStops(
  orderedPaths: readonly string[],
  findings: readonly ReviewFinding[],
  humanThreads: readonly HumanOpenThread[] = [],
): TourStop[] {
  const byPath = new Map<string, ReviewFinding[]>();
  const findingAnnotationIds = new Set<string>();
  for (const f of findings) {
    const list = byPath.get(f.location.path);
    if (list) list.push(f);
    else byPath.set(f.location.path, [f]);
    findingAnnotationIds.add(f.annotation_id);
  }
  for (const list of byPath.values()) {
    list.sort(
      (a, b) => severityRank(a.severity) - severityRank(b.severity) || a.created_at - b.created_at,
    );
  }

  const threadsByPath = new Map<string, HumanOpenThread[]>();
  for (const t of humanThreads) {
    // Already a finding-stop above — never a second stop for the same
    // annotation id.
    if (findingAnnotationIds.has(t.id)) continue;
    const list = threadsByPath.get(t.path);
    if (list) list.push(t);
    else threadsByPath.set(t.path, [t]);
  }
  for (const list of threadsByPath.values()) {
    list.sort((a, b) => a.createdAt - b.createdAt);
  }

  const stops: TourStop[] = [];
  for (const path of orderedPaths) {
    const fs = byPath.get(path);
    const ts = threadsByPath.get(path);
    if ((!fs || fs.length === 0) && (!ts || ts.length === 0)) {
      stops.push({ path });
      continue;
    }
    for (const f of fs ?? []) {
      stops.push({ path, findingSlug: f.slug, annotationId: f.annotation_id });
    }
    for (const t of ts ?? []) {
      stops.push({ path, annotationId: t.id });
    }
  }
  return stops;
}

/// A stable per-stop identity for React keys / equality checks — `path`
/// alone for a plain stop, `path#slug` for a finding stop (a slug is
/// unique per review, so this never collides across files), else (V80-M4, a
/// thread stop with no finding slug) `path#annotationId`.
export function tourStopKey(stop: TourStop): string {
  if (stop.findingSlug) return `${stop.path}#${stop.findingSlug}`;
  if (stop.annotationId) return `${stop.path}#${stop.annotationId}`;
  return stop.path;
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
