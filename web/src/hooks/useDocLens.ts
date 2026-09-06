// DCB W1.D — kb-code's cross-origin doc-lens surface. The FOURTH documented
// #23 exception (see queryClient.ts): finite staleTime (a live git-tree
// read the SSE bridge structurally cannot invalidate — kb-code is a
// different daemon with no wiring into kb's `sse` facade), a manual refresh
// affordance next to the rendered `resolved_unix`, no SSE subscription of
// any kind. `retry: false` overrides the global default — a network/CORS
// failure against a down or unreachable kb-code will not spontaneously
// succeed on one extra attempt, and immediate failure gets the degrade UI
// on screen faster.

import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import {
  fetchDocLens,
  fetchDocLensScorecard,
  putDocLensPin,
} from "../api/doclens";
import { toast } from "../lib/toast";

const DOCLENS_STALE_MS = 30_000;

export function useDocLensScorecard(
  codeUrl: string | null,
  kb: string,
  docId: string,
  enabled: boolean,
) {
  return useQuery({
    queryKey: ["doclensScorecard", kb, docId],
    enabled: enabled && !!codeUrl,
    queryFn: ({ signal }) => fetchDocLensScorecard(codeUrl!, kb, docId, signal),
    staleTime: DOCLENS_STALE_MS,
    retry: false,
  });
}

export function useDocLens(
  codeUrl: string | null,
  kb: string,
  docId: string,
  repo: string | null,
) {
  return useQuery({
    // `repo` is PART of the key — picking a different checkout is a
    // different query, fetched automatically on key change.
    queryKey: ["doclens", kb, docId, repo ?? ""],
    enabled: !!codeUrl && !!repo,
    queryFn: ({ signal }) => fetchDocLens(codeUrl!, kb, docId, repo!, signal),
    staleTime: DOCLENS_STALE_MS,
    retry: false,
  });
}

/// R5 — the pin write this track ships. On success, invalidate the
/// scorecard query so a re-render picks up the new `pinned_repo` without a
/// manual refetch() call. `onError` (W1.D.R #1) — a failed PUT was
/// previously fully swallowed (no `onError` at all): the repo pick still
/// applies locally (the caller's `setRepoParam` already ran before
/// `.mutate()`, independent of this outcome), but the persistence failure
/// itself was invisible. Surface it — the next fresh visit to this doc
/// silently won't remember the pick, which is worth knowing about now.
export function useSetDocLensPin(
  codeUrl: string | null,
  kb: string,
  docId: string,
) {
  const qc = useQueryClient();
  return useMutation({
    mutationFn: (args: { repo: string; docHash: string | null }) =>
      putDocLensPin(codeUrl!, kb, docId, args.repo, args.docHash),
    onSuccess: () => {
      void qc.invalidateQueries({ queryKey: ["doclensScorecard", kb, docId] });
    },
    onError: (err) => {
      toast.err(
        `couldn't remember this checkout: ${err instanceof Error ? err.message : String(err)}`,
      );
    },
  });
}
