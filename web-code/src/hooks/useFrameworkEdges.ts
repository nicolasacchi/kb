// PRR-N5 (T1) — `components/provenance/FrameworkCard.tsx`'s data hook.
// `GET /api/framework/edges` is a same-origin, plain `auth_bearer` read
// (kb-code is THIS daemon's own frontend, same reasoning `hooks/
// useDocRefs.ts`'s header doc gives for its sibling read) over a per-file
// rails-lens result that changes only on a re-ingest pass, not per request —
// same "doclens-style" finite-staleTime, no-SSE-tie shape `useDocRefs.ts`
// uses (`rails_edges` has no dedicated SSE event of its own either).
// `retry: false` for the same reason that hook gives: a miss here is a
// repo-config/lens-eligibility fact (not-a-Rails-repo, no rails-lens pass
// yet), not a transient failure worth React Query's default retry.
import { useQuery } from "@tanstack/react-query";
import { fetchFrameworkEdges } from "../api/client";

const FRAMEWORK_EDGES_STALE_MS = 30_000;

export function frameworkEdgesQueryKey(repo: string | undefined, path: string | undefined) {
  return ["framework", repo, path] as const;
}

/// `enabled` only once both `repo`/`path` are known — mirrors
/// `useDocRefs`'s own gate (a caller with `path === undefined`, i.e. no file
/// open, gets `enabled: false` for free, no separate "is a file open" flag).
export function useFrameworkEdges(repo: string | undefined, path: string | undefined) {
  return useQuery({
    queryKey: frameworkEdgesQueryKey(repo, path),
    queryFn: () => fetchFrameworkEdges(repo as string, path as string),
    enabled: repo !== undefined && path !== undefined && path !== "",
    staleTime: FRAMEWORK_EDGES_STALE_MS,
    retry: false,
  });
}
