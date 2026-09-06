import { useQuery } from "@tanstack/react-query";
import { fetchMemoriesFrom, type MemoryFromRow } from "../api/client";

// CT-A1 (U3 parse-back) — every memory highlighted FROM one artifact (the
// reverse of `MemoryProvenance`). Keyed under the existing `["memories", …]`
// prefix (`api/queryClient.ts`) so a NEW highlight (`memory.ingested`,
// already bridged) refreshes this for free — no new SSE wiring
// (invariant #23/#24). `staleTime: Infinity`, same as every other TanStack
// query in this tree.
export type UseMemoriesFrom = {
  rows: MemoryFromRow[];
  loading: boolean;
  error: string | null;
};

export function useMemoriesFrom(
  kb: string | null,
  id: string | null,
): UseMemoriesFrom {
  const enabled = !!kb && !!id;
  const q = useQuery({
    queryKey: ["memories", "from", kb ?? "", id ?? ""],
    enabled,
    queryFn: ({ signal }) => fetchMemoriesFrom(kb as string, id as string, signal),
    staleTime: Infinity,
  });
  return {
    rows: q.data?.rows ?? [],
    loading: q.isLoading,
    error: q.error
      ? q.error instanceof Error
        ? q.error.message
        : String(q.error)
      : null,
  };
}
