// PRR-U9 — diagnostics through lip (design-addendum-2.md §D). `GET
// /api/diagnostics` is a same-origin, plain `auth_bearer` read (kb-code is
// THIS daemon's own frontend, same reasoning `hooks/useFrameworkEdges.ts`'s
// header doc gives for its own sibling read) over a result that's
// computed fresh per request and never persisted server-side
// (`lip.rs::diagnostics_route`'s own doc) — it depends on the provider's
// LIVE state, which fires no SSE event this app subscribes to. Same
// "doclens-style" finite-staleTime, no-SSE-tie shape `useDocLens.ts`/
// `useFrameworkEdges.ts` already use (root CLAUDE.md invariant #23's
// no-SSE-tie exception set).
//
// `path` alone (not `blobHash`) drives the query key's refetch-on-file-
// change — TanStack Query treats a key change as "load fresh", so
// switching files naturally refetches without any extra plumbing.

import { useQuery } from "@tanstack/react-query";
import { fetchDiagnostics } from "../api/client";
import { langCoveredByIntel } from "../lib/diagnostics";
import { useRepos } from "./useRepos";

const DIAGNOSTICS_STALE_MS = 30_000;

export function diagnosticsQueryKey(repo: string | undefined, path: string | undefined) {
  return ["diagnostics", repo, path] as const;
}

/// Gated on `GET /api/repos`' per-repo `intel` field (`RepoIntelStatus`)
/// covering `path`'s detected language (`lib/diagnostics.ts`'s
/// `langCoveredByIntel`) — a repo with no configured lip/1 provider, or one
/// that doesn't cover this file's language, never fires the request at
/// all (the "no provider → card absent entirely" named state starts here,
/// not in the render layer). `retry: false`, same reasoning `useFrameworkEdges`
/// gives for its own lens fetch: a miss here is a repo-config fact, not a
/// transient failure worth React Query's default retry.
export function useDiagnostics(repo: string | undefined, path: string | undefined) {
  const { data: reposData } = useRepos();
  const intel = reposData?.repos.find((r) => r.name === repo)?.intel ?? null;
  const covered = langCoveredByIntel(path, intel);
  const query = useQuery({
    queryKey: diagnosticsQueryKey(repo, path),
    queryFn: () => fetchDiagnostics(repo as string, path as string),
    enabled: repo !== undefined && path !== undefined && path !== "" && covered,
    staleTime: DIAGNOSTICS_STALE_MS,
    retry: false,
  });
  return { ...query, covered };
}
