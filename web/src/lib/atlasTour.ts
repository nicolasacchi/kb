// W3.F-c — LOCI TOURS: a memory-palace walk through the atlas.
//
// A tour is NOT a new noun, and this file has no storage of any kind.
// kb has exactly ONE durable collection type — kb-list/1 — and it is
// already everything a tour needs:
//
//   * ORDERED. `kb_core::lists` keeps a dense 0-based `position` per entry
//     and `kb list move` reorders it; the walk order IS the list order.
//   * TRAVERSABLE. `?list=&entry=` already rides the reader URL and
//     `components/lists/QueueBar.tsx` already owns prev/next/exit over it.
//   * PROGRESS-BEARING. Read state is DERIVED per response (invariant
//     #25) — override > section dwell > scroll completion — so there is
//     nothing left for a tour to record. There is deliberately NO tour
//     progress store, NO completion percentage, NO "tour finished" state
//     and NO streak: that is a non-goal boundary (README → Non-goals), not
//     a styling choice. Read state is the only progress signal shown.
//
// So the ONLY thing missing was navigation, and that is all this module
// is: an ordered list of entries plus the map of where their artifacts
// were drawn ⇒ a camera-stop sequence. Pure (no React, no fetch, no DOM),
// colocated vitest, same shape as atlasFit.ts / atlasSelection.ts.

/** The subset of `ListEntry` a stop needs — a structural type (like
 * `canvas.ts`'s `LayoutEntry`) so the pure module doesn't depend on the
 * generated wire binding. A real `ListEntry` is assignable to it. */
export type TourEntry = {
  id: string;
  artifact_id: string;
  source_relative?: string | null;
  tombstone?: boolean | null;
  title?: string | null;
};

/** Where the atlas drew an artifact — AtlasView's logical (W×H) coords. */
export type TourPoint = { x: number; y: number };

/** One camera stop. `entryId` is the kb-list/1 entry the reader hands off
 * to (`entryTrailHref(kb, listId, entry)` — the ONE trail-href builder);
 * `index` is the stop's position IN THE TOUR, not in the list, because
 * skipped entries would otherwise leave gaps in a "stop 3 of 7" counter. */
export type TourStop = {
  entryId: string;
  artifactId: string;
  label: string;
  x: number;
  y: number;
  index: number;
};

/** Turn an ORDERED list of entries into the camera-stop sequence.
 *
 * Skipping matches `QueueBar`'s `nextReadable` exactly — an entry is a
 * stop only when it is not tombstoned AND has a `source_relative` (there
 * is no artifact to land on otherwise) — plus the one condition a map
 * adds: the artifact must actually be drawn on this atlas. A dot the
 * renderer doesn't have (a doc past the loaded point set, or one filtered
 * out of the current view) has no position to fly to, so it is skipped
 * rather than approximated.
 *
 * Order is preserved verbatim (the caller passes entries in `position`
 * order; nothing here sorts), and the function is pure — same input, same
 * output, every time. */
export function tourStops(
  entries: readonly TourEntry[],
  placedById: ReadonlyMap<string, TourPoint>,
): TourStop[] {
  const out: TourStop[] = [];
  for (const e of entries) {
    if (e.tombstone) continue;
    if (!e.source_relative) continue;
    const p = placedById.get(e.artifact_id);
    if (!p) continue;
    out.push({
      entryId: e.id,
      artifactId: e.artifact_id,
      label: e.title || e.source_relative,
      x: p.x,
      y: p.y,
      index: out.length,
    });
  }
  return out;
}

/** How many of `entries` a tour cannot visit — the honest counterpart to
 * `tourStops().length`, so the UI can say "3 of 7 entries aren't on this
 * map" instead of silently walking a shorter path than the list. */
export function tourSkippedCount(
  entries: readonly TourEntry[],
  placedById: ReadonlyMap<string, TourPoint>,
): number {
  return entries.length - tourStops(entries, placedById).length;
}

/** Step the stop index, clamped to the sequence (no wrap — a walk has a
 * direction, exactly like the QueueBar's prev/next). Returns the current
 * index unchanged at either end, and `0` for an empty sequence. */
export function stepStop(
  stops: readonly TourStop[],
  current: number,
  dir: -1 | 1,
): number {
  if (stops.length === 0) return 0;
  const next = current + dir;
  if (next < 0) return 0;
  if (next > stops.length - 1) return stops.length - 1;
  return next;
}
