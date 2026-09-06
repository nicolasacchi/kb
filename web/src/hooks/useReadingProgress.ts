import { useQuery } from "@tanstack/react-query";
import { fetchHistory, type HistoryEntry } from "../api/history";

export type Progress = { pct: number; isDone: boolean };

const DONE_THRESHOLD = 95;
const FETCH_LIMIT = 200;
const EMPTY_MAP = new Map<string, Progress>();

// Latest-visit reading progress per artifact (G5). Built from a single
// fetchHistory({kind:"open"}) call; client-side dedup by artifact_id
// (newest-first wins) yields the per-artifact "most recent" scroll
// position. The SSE bridge invalidates ["readingProgress", kb] on
// `history.recorded` (and gap resync) so newly-opened artifacts show
// their progress as soon as a visit lands.
//
// Returns an empty Map until the first fetch resolves, so callers treat
// `progress.get(id) === undefined` identically to "no progress
// recorded" — the chip doesn't render either way. No "0%" noise.
//
// Per H6: scroll-only updates do NOT emit `history.recorded` (visit
// INSERTs do), so within a session the chip lags behind active scroll
// until a new visit fires. Glanceable signal, not a live readout.
//
// K12 (re-render stability, formerly a hand-rolled store): the gallery
// + detail mounts share ONE cache entry; the queryFn returns the raw
// entry rows so react-query's structural sharing keeps the data
// reference stable when a refetch returns identical content, and the
// module-level `select` is memoized on that reference — so the Map
// identity only changes when the progress actually changed, and Cards
// don't re-render on no-op refetches.

function recompute(entries: HistoryEntry[]): Map<string, Progress> {
  const next = new Map<string, Progress>();
  for (const e of entries) {
    if (!e.artifact_id) continue;
    if (next.has(e.artifact_id)) continue;
    if (e.scroll_max <= 0) continue;
    // Use the per-visit high-water mark (V0007) so a fully-read ✓
    // stays sticky when the reader scrolls back up to revisit an
    // earlier section. scroll_y itself is still authoritative for
    // resume — see useReview/iframe runtime.
    const raw = (e.scroll_y_max / e.scroll_max) * 100;
    const pct = Math.max(0, Math.min(100, Math.round(raw)));
    next.set(e.artifact_id, { pct, isDone: pct >= DONE_THRESHOLD });
  }
  return next;
}

export function useReadingProgress(
  kb: string | undefined,
): Map<string, Progress> {
  const q = useQuery({
    queryKey: ["readingProgress", kb] as const,
    enabled: !!kb,
    queryFn: ({ signal }) =>
      fetchHistory(kb as string, { kind: "open", limit: FETCH_LIMIT, signal }),
    select: recompute,
  });
  return q.data ?? EMPTY_MAP;
}
