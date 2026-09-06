import { useQuery } from "@tanstack/react-query";
import { fetchResurface } from "../api/client";

// Resurface queue for the gallery strip (invariant #23 shape: a bare
// TanStack query, staleTime Infinity; the SSE bridge invalidates
// ["resurface", kb] on comments.updated / history.recorded — reading-progress
// beacons carry no SSE by design (#19), so the read term refreshes on the
// next natural invalidation, which is the right cadence for a calm surface).
export function useResurface(kb: string | null, enabled = true) {
  return useQuery({
    queryKey: ["resurface", kb],
    enabled: enabled && !!kb,
    queryFn: ({ signal }) => fetchResurface(kb!, signal),
    staleTime: Infinity,
  });
}
