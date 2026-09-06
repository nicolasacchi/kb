import { useQuery } from "@tanstack/react-query";
import { fetchDiff } from "../api/client";

export function useDiff(
  repo: string | undefined,
  path: string | undefined,
  from: string | undefined,
  to: string | undefined,
) {
  return useQuery({
    queryKey: ["diff", repo, path, from, to ?? null],
    queryFn: () => fetchDiff(repo as string, path as string, from as string, to),
    enabled: repo !== undefined && path !== undefined && from !== undefined,
  });
}
