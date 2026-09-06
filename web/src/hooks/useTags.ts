import { useQuery } from "@tanstack/react-query";
import { fetchTags, type TagSummary } from "../api/client";

const EMPTY: TagSummary[] = [];

// FS3 — shared tag-facet list for a kb (gallery sidebar + search rail).
// Keyed ["tags", kb] so both surfaces share one cache entry; the SSE
// bridge (queryClient.ts docsGate) invalidates it on (re)index churn.
// staleTime is the global Infinity — tags only change when artifacts are
// indexed, which the bridge already covers.
export function useTags(kb: string | undefined): TagSummary[] {
  const q = useQuery({
    queryKey: ["tags", kb] as const,
    enabled: !!kb,
    queryFn: ({ signal }) => fetchTags(kb as string, signal),
  });
  return q.data ?? EMPTY;
}
