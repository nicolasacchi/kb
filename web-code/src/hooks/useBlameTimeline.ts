import { useQuery } from "@tanstack/react-query";
import { fetchBlameTimeline } from "../api/client";

/// The why-panel's "timeline" expander (W4.4, step 3): `GET
/// /api/blame/timeline`'s set-valued line history, fetched only once the
/// expander is actually opened (`enabled`) — a bounded `git log -L` call,
/// but no reason to pay for it before the user asks.
export function useBlameTimeline(
  repo: string | undefined,
  path: string | undefined,
  line: number | undefined,
  enabled: boolean,
) {
  return useQuery({
    queryKey: ["blame-timeline", repo, path, line],
    queryFn: () => fetchBlameTimeline(repo as string, path as string, line as number),
    enabled: enabled && repo !== undefined && path !== undefined && line !== undefined,
  });
}
