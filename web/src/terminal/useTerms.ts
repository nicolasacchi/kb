// T14 — the `["terms"]` query: one finite-staleTime read of the
// terminal list. Ledger entry in `web/src/api/queryClient.ts` (the
// next documented #23 exception: 15 s staleTime plus direct
// invalidation on `term.created|term.ended|term.state`, which fire
// only on a lifecycle/state CHANGE, plus one refetch when the tab becomes
// visible again and one debounced invalidation per grid-change frame the
// open panel sees). The query never polls.
import { useQuery } from "@tanstack/react-query";
import { listTerms, type TermRow } from "../api/terms";

export function useTerms(): { terms: TermRow[]; isLoading: boolean } {
  const q = useQuery({
    queryKey: ["terms"] as const,
    queryFn: ({ signal }) => listTerms(signal),
    staleTime: 15_000,
    // A grid owner can change while this tab was hidden (another tab, a local
    // attach): one refetch when the tab is visible again, only if the row is
    // older than staleTime. Not polling; the global default is off.
    refetchOnWindowFocus: true,
  });
  return { terms: q.data ?? [], isLoading: q.isPending };
}
