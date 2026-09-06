import { useQuery } from "@tanstack/react-query";
import { fetchBlame } from "../api/client";

/// `GET /api/blame` for the whole file — gated on `enabled` so it's only
/// fetched once the reader's "provenance" toggle is on (W4.4's gutter is
/// opt-in, not fetched on every file load).
export function useBlame(
  repo: string | undefined,
  path: string | undefined,
  ref: string | undefined,
  enabled: boolean,
) {
  return useQuery({
    queryKey: ["blame", repo, path, ref ?? null],
    queryFn: () => fetchBlame(repo as string, path as string, ref),
    enabled: enabled && repo !== undefined && path !== undefined,
  });
}
