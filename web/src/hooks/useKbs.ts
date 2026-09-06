import { useQuery } from "@tanstack/react-query";
import { fetchKbs } from "../api/client";

// Shared kb-list query. One cached ["kbs"] entry across the whole app —
// dedupes the formerly ad-hoc fetchKbs() callers and, crucially, gives every
// "active kb" derivation a STABLE order: the kbs[0] fallback no longer shifts
// when an independent fetch happens to resolve in a different order
// mid-session. The kb set is fixed at daemon start (adding a kb is a config
// edit + restart, invariant #13), so the staleTime: Infinity default in
// api/queryClient.ts is exactly right and no SSE-bridge invalidation is
// needed. The ["kbs"] key matches the one routes/lists.tsx already uses.
export function useKbs() {
  return useQuery({
    queryKey: ["kbs"] as const,
    queryFn: ({ signal }) => fetchKbs(signal),
  });
}
