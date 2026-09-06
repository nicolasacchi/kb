import { useQuery } from "@tanstack/react-query";
import { fetchCompare } from "../api/client";

/// `GET /api/compare?repo=&from=&to=&three_dot=[&attribution=true]` —
/// repo-level compare (Phase C2), plus Phase G3's per-commit join-ladder
/// attribution (`attribution` defaults to `true`: session-grouped review is
/// the Compare page's headline feature now, so every caller wants it —
/// pass `false` explicitly for the rare case that doesn't).
export function useCompare(
  repo: string | undefined,
  from: string | undefined,
  to: string | undefined,
  threeDot: boolean,
  attribution = true,
) {
  return useQuery({
    queryKey: ["compare", repo, from, to, threeDot, attribution],
    queryFn: () => fetchCompare(repo as string, from as string, to as string, threeDot, attribution),
    enabled: repo !== undefined && !!from && !!to,
  });
}
