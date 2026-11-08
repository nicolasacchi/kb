// A5 — kb-code's cross-origin `kbc-session-card/1` read for the session
// page's code-lane section. Joins the doclens no-SSE-tie set (see
// `hooks/useDocLens.ts` and `api/queryClient.ts`): kb-code is a SEPARATE
// daemon on a different origin with no connection to kb's `sse` facade,
// so there is no event to listen to — a FINITE staleTime, `retry: false`
// (a down or unreachable kb-code will not spontaneously succeed on one
// extra attempt, and immediate failure gets the degrade UI on screen
// faster), and no SSE subscription of any kind. The `repo` slot is part
// of the key: a different filter is a different query, fetched
// automatically on key change.

import { useQuery } from "@tanstack/react-query";
import { fetchSessionCard } from "../api/sessionCard";

export const CODE_LANE_STALE_MS = 30_000;

export function useCodeLane(
  codeUrl: string | null,
  sid: string,
  repo?: string,
) {
  return useQuery({
    queryKey: ["sessionCard", "code", sid, repo ?? ""],
    queryFn: ({ signal }) => fetchSessionCard(codeUrl!, sid, repo, signal),
    enabled: !!codeUrl && !!sid,
    staleTime: CODE_LANE_STALE_MS,
    retry: false,
  });
}
