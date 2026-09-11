import { useQuery } from "@tanstack/react-query";
import { fetchFileStops } from "../api/client";

/// `GET /api/file/stops` — gated on `enabled` (the scrubber strip is opt-in).
export function useFileStops(
  repo: string | undefined,
  path: string | undefined,
  enabled: boolean,
) {
  return useQuery({
    queryKey: ["file-stops", repo, path],
    queryFn: () => fetchFileStops(repo as string, path as string),
    enabled: enabled && repo !== undefined && path !== undefined,
  });
}
