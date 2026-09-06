import { useCallback } from "react";
import { useQuery, useQueryClient } from "@tanstack/react-query";
import { fetchRecall, type RecallHit } from "../api/client";

// v0.22 — related-memories for the reader rail, SHARED by the Sessions-tab
// panel (the list) and the rail's "memories" count badge so they fetch ONCE.
// TanStack-cached (keyed under the ["memories", …] prefix the SSE bridge
// already invalidates on every memory.* / session.captured / artifact.removed
// event — invariant #23/#24, no manual EventSource). The badge mounts this
// always (it lives on the persistent rail), so the count is available
// regardless of which sub-tab is open.
//
// `query` re-anchors the recall to a specific artifact (D3 passes the doc
// title); empty/undefined = the kb-wide recency list (the original behaviour).
const RELATED_LIMIT = 12;

export type UseRelatedMemories = {
  hits: RecallHit[];
  count: number;
  loading: boolean;
  error: string | null;
  /// Force a refetch (used by the inline pin/unpin actions; the SSE bridge
  /// also refreshes on memory.linked/unlinked, so this is belt-and-suspenders).
  refresh: () => void;
};

export function useRelatedMemories(
  kb: string | null,
  query?: string,
): UseRelatedMemories {
  const qc = useQueryClient();
  const q = useQuery({
    queryKey: ["memories", "related", kb ?? "", query ?? ""],
    enabled: !!kb,
    queryFn: ({ signal }) =>
      fetchRecall(
        { scope: "all", forKb: kb!, q: query || undefined, limit: RELATED_LIMIT },
        signal,
      ),
    staleTime: Infinity,
  });
  const refresh = useCallback(() => {
    void qc.invalidateQueries({ queryKey: ["memories"] });
  }, [qc]);
  return {
    hits: q.data?.hits ?? [],
    count: q.data?.hits.length ?? 0,
    loading: q.isLoading,
    error: q.error
      ? q.error instanceof Error
        ? q.error.message
        : String(q.error)
      : null,
    refresh,
  };
}
