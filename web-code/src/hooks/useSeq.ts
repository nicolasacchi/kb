import { useQuery } from "@tanstack/react-query";
import { fetchSeq } from "../api/client";

export function seqQueryKey(repo: string | undefined) {
  return ["seq", repo ?? null] as const;
}

/// `GET /api/seq?repo=` — kbc-seq/1. Counts on the Collections page only;
/// there is no SSE event for sets/boards changing, so the result goes stale
/// after 30 s instead of living forever like the SSE-bridged queries.
export function useSeq(repo: string | undefined) {
  return useQuery({
    queryKey: seqQueryKey(repo),
    queryFn: () => fetchSeq(repo as string),
    enabled: repo !== undefined && repo !== "",
    staleTime: 30_000,
  });
}
