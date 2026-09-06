import { useQuery } from "@tanstack/react-query";
import { fetchCommit } from "../api/client";

/// `GET /api/commit?repo=&sha=` — the commit page hub (Phase C1).
export function useCommit(repo: string | undefined, sha: string | undefined) {
  return useQuery({
    queryKey: ["commit", repo, sha],
    queryFn: () => fetchCommit(repo as string, sha as string),
    enabled: repo !== undefined && sha !== undefined && sha !== "",
  });
}
