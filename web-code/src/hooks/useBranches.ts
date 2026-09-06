import { useQuery } from "@tanstack/react-query";
import { fetchBranches } from "../api/client";

export type BranchSort = "name" | "suggested";

/// `GET /api/branches?repo=` — every branch's ahead/behind + tip
/// attribution vs. the default branch (Phase C3). `sort` is forwarded as
/// `?sort=` (V4.L1); `repo` stays query-key index 1 so prefix invalidation
/// on `["branches", repo]` still matches every sort variant.
export function useBranches(repo: string | undefined, sort?: BranchSort) {
  return useQuery({
    queryKey: ["branches", repo, sort],
    queryFn: () => fetchBranches(repo as string, sort),
    enabled: repo !== undefined,
  });
}
