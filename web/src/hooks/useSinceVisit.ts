// OL IDEA2 — the "since your last visit" answer for the open artifact.
//
// An ON-OPEN answer: one fetch per reader mount (`refetchOnMount: "always"`),
// `staleTime: Infinity`, deliberately NOT bridged to SSE (queryClient.ts key
// contract, "Deliberately NOT bridged"). A change that lands while the artifact
// is open already live-reloads the page and shows in the banner on the next
// open; refetching on churn would flag the operator's own note autosaves as
// changes on every save.

import { useQuery } from "@tanstack/react-query";
import { fetchSinceVisit, type SinceVisitResponse } from "../api/versions";

export function sinceVisitKey(kb: string, id: string) {
  return ["sinceVisit", kb, id] as const;
}

export function useSinceVisit(
  kb: string,
  id: string,
  enabled: boolean,
): SinceVisitResponse | null {
  const q = useQuery({
    queryKey: sinceVisitKey(kb, id),
    enabled: enabled && !!kb && !!id,
    queryFn: ({ signal }) => fetchSinceVisit(kb, id, signal),
    staleTime: Infinity,
    refetchOnMount: "always",
    retry: false,
  });
  return q.data ?? null;
}
