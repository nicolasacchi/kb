import { useQuery } from "@tanstack/react-query";
import { fetchRepoState } from "../api/client";

/// `GET /api/repo-state?repo=` (Phase G2) — this repo's current git
/// operation state (rebase/merge/cherry-pick/bisect in flight, conflicted
/// paths, working-tree dirtiness). Query key `["repo-state", repo]` matches
/// `api/queryClient.ts`'s generic per-repo invalidation predicate
/// (`queryKey[1] === repo`) with no bridge changes needed — both
/// `mirror.updated` and `repo.head_moved` SSE events for this repo already
/// invalidate (and refetch) it, same as `["branches", repo]`/`["compare",
/// …]` do (see that module's "query-key conventions" list).
export function useRepoState(repo: string | undefined) {
  return useQuery({
    queryKey: ["repo-state", repo],
    queryFn: () => fetchRepoState(repo as string),
    enabled: repo !== undefined,
  });
}
