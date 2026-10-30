// OL IDEA1 - the sessions kb reports as blocked on the operator, for Home's
// "Needs you" line. `GET /api/kb/blocked` is a relay of kb's live-status; kb
// has no event that reaches this daemon, so there is no SSE tie to bridge.
// That makes this a #23-style polling exception (web-code/CLAUDE.md, "The
// blocked line"): a finite `staleTime`, a `refetchInterval` of the same
// length, and TanStack's default `refetchIntervalInBackground: false`, so a
// hidden tab does not poll. It is mounted ONLY by `HomeNeedsYou`, so it polls
// only while Home is open.

import { useQuery } from "@tanstack/react-query";
import { fetchKbBlocked } from "../api/client";

export const KB_BLOCKED_KEY = ["kb", "blocked"] as const;
export const KB_BLOCKED_POLL_MS = 15_000;

/// Exported so a test can pin the polling contract without mounting a tree.
export const kbBlockedQueryOptions = {
  queryKey: KB_BLOCKED_KEY,
  queryFn: ({ signal }: { signal?: AbortSignal }) => fetchKbBlocked(signal),
  staleTime: KB_BLOCKED_POLL_MS,
  refetchInterval: KB_BLOCKED_POLL_MS,
} as const;

export function useKbBlocked() {
  return useQuery(kbBlockedQueryOptions);
}
