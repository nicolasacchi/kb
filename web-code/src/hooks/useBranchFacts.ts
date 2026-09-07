import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import {
  fetchBranchConflicts,
  fetchBranchFacts,
  fetchBranchFavourites,
  setBranchFavourite,
  startBranchReview,
  type BranchFactsQuery,
} from "../api/client";

// V75-M3 — `branch-facts/1` and its three siblings.
//
// Query keys start `["branch-facts", repo, …]` / `["branch-conflicts", …]` /
// `["branch-favourites", repo]` so the SSE bridge's generic per-repo
// predicate (`queryKey[1] === repo`) invalidates all three on the
// `branch.changed` event the favourites route emits — the convention
// `api/queryClient.ts`'s own key registry documents.

export function branchFactsQueryKey(repo: string | undefined, q: BranchFactsQuery) {
  return [
    "branch-facts",
    repo,
    q.view ?? null,
    q.q ?? null,
    q.prefix ?? null,
    q.fav ?? false,
    q.limit ?? null,
    q.offset ?? null,
    q.pr ?? false,
    q.ci ?? false,
    q.patchId ?? false,
  ] as const;
}

/// `GET /api/branches/facts`. Every knob is in the key, so switching view
/// or typing a filter fetches its OWN cache entry — a stale row-set never
/// bleeds across a selection (the `useTodos` precedent).
export function useBranchFacts(repo: string | undefined, query: BranchFactsQuery) {
  return useQuery({
    queryKey: branchFactsQueryKey(repo, query),
    queryFn: () => fetchBranchFacts(repo as string, query),
    enabled: repo !== undefined && repo !== "",
  });
}

/// `GET /api/branches/conflicts`. `enabled` is the panel's own open state:
/// the radar is up to 40 `merge-tree` runs, so it fires only when a caller
/// actually asked for it — never on page load.
export function useBranchConflicts(
  repo: string | undefined,
  against: string | null,
  opts: { limit?: number; q?: string } = {},
) {
  return useQuery({
    queryKey: ["branch-conflicts", repo, against, opts.limit ?? null, opts.q ?? null],
    queryFn: () => fetchBranchConflicts(repo as string, against as string, opts.limit, opts.q),
    enabled: repo !== undefined && repo !== "" && !!against,
    // The radar's cost is server-side and its answer moves only when a ref
    // does; one retry on a 5xx is plenty, and a 400 (the `touches:`
    // refusal) must surface immediately rather than being retried.
    retry: false,
  });
}

export function useBranchFavourites(repo: string | undefined) {
  return useQuery({
    queryKey: ["branch-favourites", repo],
    queryFn: () => fetchBranchFavourites(repo as string),
    enabled: repo !== undefined && repo !== "",
  });
}

/// Star/unstar. Invalidates BOTH the favourites list and every facts page
/// for this repo — the star is rendered on the row, so a facts page that
/// kept its old `favourite` would show the opposite of what the list says.
export function useSetBranchFavourite(repo: string | undefined) {
  const qc = useQueryClient();
  return useMutation({
    mutationFn: ({ ref, on }: { ref: string; on: boolean }) =>
      setBranchFavourite(repo as string, ref, on),
    onSuccess: () => {
      qc.invalidateQueries({ queryKey: ["branch-favourites", repo] });
      qc.invalidateQueries({ queryKey: ["branch-facts", repo] });
    },
  });
}

/// `POST /api/branches/review` — loopback-only server-side. The caller
/// gates the affordance on `GET /api/repos`'s `loopback` bool; this hook
/// does not, because a hook that hid its own failure would make the
/// refusal invisible.
export function useStartBranchReview(repo: string | undefined) {
  const qc = useQueryClient();
  return useMutation({
    mutationFn: ({ ref, base, title }: { ref: string; base: string; title?: string }) =>
      startBranchReview(repo as string, ref, base, title),
    onSuccess: () => {
      qc.invalidateQueries({ queryKey: ["reviews", repo] });
      qc.invalidateQueries({ queryKey: ["branch-facts", repo] });
    },
  });
}
