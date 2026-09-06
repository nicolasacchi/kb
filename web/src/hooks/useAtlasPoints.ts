import { useQuery } from "@tanstack/react-query";
import { fetchAtlasPoints, type AtlasPointsResponse } from "../api/client";

// W3.M-c — the FULL-corpus atlas point set (`GET /api/kb/{kb}/atlas/points`,
// M-a), the fix for the measured defect where the atlas view only ever drew
// the gallery's first `useDocs` page (pageSize 200, no `loadMore` on the
// atlas branch) while its own status line claimed the full artifact count.
//
// Query-key convention (documented in api/queryClient.ts): `["atlasPoints",
// kb]`, staleTime Infinity like every other server-state key (#23) — the SSE
// bridge invalidates it on `atlas.recompute.complete` / `atlas.recluster.
// complete`, mirroring the existing `["atlasLabels", kb]` entry. No
// fetch/subscribe plumbing here beyond the standard client call; no new SSE
// connection (#24) — the bridge in queryClient.ts owns that.
export function useAtlasPoints(kb: string | undefined) {
  return useQuery({
    queryKey: ["atlasPoints", kb] as const,
    enabled: !!kb,
    queryFn: ({ signal }) => fetchAtlasPoints(kb as string, signal),
  });
}

export type { AtlasPointsResponse };
