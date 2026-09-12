import { useQuery } from "@tanstack/react-query";
import { fetchFileStops } from "../api/client";

/// `GET /api/file/stops` — gated on `enabled` (the scrubber strip is opt-in).
export function useFileStops(
  repo: string | undefined,
  path: string | undefined,
  enabled: boolean,
  ref?: string,
) {
  return useQuery({
    queryKey: ["file-stops", repo, path, ref ?? null],
    queryFn: () => fetchFileStops(repo as string, path as string, undefined, undefined, ref),
    enabled: enabled && repo !== undefined && path !== undefined,
  });
}
