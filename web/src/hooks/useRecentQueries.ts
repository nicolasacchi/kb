import { useQuery } from "@tanstack/react-query";
import { fetchQueries, type QueryEntry } from "../api/client";

const EMPTY: QueryEntry[] = [];

// FS8 — recent search queries for a kb, from the per-kb queries ring
// (`/api/kb/{kb}/queries`). Used by the saved/recent dropdown under the
// search box. `enabled` gates the fetch to when the dropdown is open, and
// staleTime 0 makes each open re-pull so the latest searches show.
export function useRecentQueries(
  kb: string | undefined,
  enabled = true,
  limit = 12,
): QueryEntry[] {
  const q = useQuery({
    queryKey: ["queries", kb, limit] as const,
    enabled: !!kb && enabled,
    queryFn: ({ signal }) => fetchQueries(kb as string, limit, signal),
    staleTime: 0,
  });
  return q.data ?? EMPTY;
}
