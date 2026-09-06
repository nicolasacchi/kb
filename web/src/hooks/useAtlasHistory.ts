import { useQuery } from "@tanstack/react-query";
import {
  fetchAtlasFrame,
  fetchAtlasHistory,
  type AtlasFrameShowResponse,
  type AtlasHistoryResponse,
} from "../api/client";

// W3.T-c — the atlas time-lapse's two server-state reads (AtlasView's frame
// scrubber). Query-key convention documented in `api/queryClient.ts`:
//
//   ["atlasHistory", kb]      the frame LIST (newest first, cheap metadata
//                             only — no points)
//   ["atlasFrame", kb, id]    ONE frame's points, already Procrustes-aligned
//                             server-side against the newest frame
//
// Both staleTime Infinity like every other server-state key (#23); both
// bridged on `atlas.snapshot.recorded` in queryClient.ts. NO fetch/subscribe
// plumbing lives here beyond the standard client call, and neither hook
// opens an SSE connection (#24) — the bridge owns invalidation.
//
// The frame body is LAZY in two senses: `useAtlasHistory` only runs when the
// caller passes a kb (AtlasView passes `undefined` until the operator opens
// the time-lapse panel, so a plain atlas visit costs zero extra requests),
// and `useAtlasFrame` only runs once a frame id is actually selected. A
// played-through time-lapse fetches each frame once and then replays out of
// the cache forever (staleTime Infinity), so only the first pass is as slow
// as the daemon answers.

export function useAtlasHistory(kb: string | undefined) {
  return useQuery({
    queryKey: ["atlasHistory", kb] as const,
    enabled: !!kb,
    queryFn: ({ signal }) => fetchAtlasHistory(kb as string, signal),
  });
}

/** One frame's aligned points. `id == null` (no frame selected yet, or the
 * kb has no frames at all) leaves the query disabled rather than guessing a
 * frame — the empty state is the honest answer, not a spinner.
 *
 * `alignTo` is left unset by default, which the route reads as "the newest
 * frame" — the common case ("how does this old frame compare to where the
 * map is now") and the one the SPA's colour remap assumes. Passing it here
 * would need its own key slot; it isn't in the key today because nothing
 * varies it. */
export function useAtlasFrame(kb: string | undefined, id: number | null) {
  return useQuery({
    queryKey: ["atlasFrame", kb, id] as const,
    enabled: !!kb && id != null,
    queryFn: ({ signal }) =>
      fetchAtlasFrame(kb as string, id as number, undefined, signal),
  });
}

export type { AtlasHistoryResponse, AtlasFrameShowResponse };
