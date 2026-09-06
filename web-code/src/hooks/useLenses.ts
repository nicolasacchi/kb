import { useQuery } from "@tanstack/react-query";
import { fetchLenses } from "../api/client";

/// `GET /api/lenses` — keyed on repo+path+blobHash when available so a
/// live-mirror blob change re-fetches; falls back to repo+path.
export function useLenses(
  repo: string | undefined,
  path: string | undefined,
  blobHash: string | undefined,
  enabled = true,
) {
  return useQuery({
    queryKey: ["lenses", repo, path, blobHash ?? null],
    queryFn: () => fetchLenses(repo as string, path as string),
    enabled: enabled && repo !== undefined && path !== undefined,
    staleTime: 60_000,
  });
}
