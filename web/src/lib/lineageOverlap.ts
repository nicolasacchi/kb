// MI-W4.3 — the lineage viewer's TimeArcs-idiom overlap window: for one
// supersede hop (an OLDER fact and the NEWER one that replaced it), how
// long did the outdated fact keep coexisting with its correction? Duration,
// not just order — a stale fact that sat around uncorrected for months
// after its replacement landed is a different (worse) story than one
// forgotten the same day.
//
// The wire carries a `forgotten` BOOLEAN, not an exact "forgotten at"
// timestamp (kb-core's `kb-forgotten-at` meta is write-only — see
// `MemoryProvenance`'s doc comment on the general write-only-metas
// pattern), so a RESOLVED overlap can't be given a precise end date; this
// module is honest about that rather than fabricating one.

export interface OverlapNode {
  createdUnix: number | null;
  forgotten: boolean;
}

export interface OverlapWindow {
  /** When the newer (corrector) memory appeared — the overlap's start. */
  startUnix: number;
  /** `true` when the older fact is STILL not forgotten — the overlap has
   * no end yet. `false` means it was eventually resolved, at some point
   * we can't pin down more precisely than "after `startUnix`". */
  ongoing: boolean;
}

/** `null` when the newer node has no resolvable creation time at all — an
 * overlap needs a start. */
export function computeOverlapWindow(
  older: OverlapNode,
  newer: OverlapNode,
): OverlapWindow | null {
  if (newer.createdUnix == null) return null;
  return { startUnix: newer.createdUnix, ongoing: !older.forgotten };
}

/** Days the overlap has spanned so far (ongoing) — `null` for a resolved
 * window (no fabricated end date; the caller should render "resolved"
 * instead of a duration). `nowUnix` is caller-supplied for testability. */
export function ongoingOverlapDays(w: OverlapWindow, nowUnix: number): number | null {
  if (!w.ongoing) return null;
  return Math.max(0, (nowUnix - w.startUnix) / 86_400);
}
