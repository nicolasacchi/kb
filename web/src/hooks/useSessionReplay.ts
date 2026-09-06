// W3.R-c — the session-replay timeline as server state.
//
// ONE plain `useQuery` (#23): no fetch plumbing, no SSE subscription, no
// lowered staleTime. The key sits under the `["sessions", …]` prefix on
// purpose — `useSessions` invalidates that whole prefix on `session.captured`
// / `session.deleted` (it owns the session.* wiring rather than the bridge,
// because a fresh capture must collapse its pagination to page 1, a semantic
// the bridge can't express), so a re-captured session's replay is invalidated
// for free wherever that hook is mounted. Nothing here opens a second SSE
// connection (#24) — replaying a FINISHED session is reading a frozen
// transcript; the surface is pull-only by design (no autoplay, no timers).
//
// The whole timeline is one payload (kb-core caps it at 2 000 beats and
// reports the truncation), and the daemon memoises the resolved build per
// (capture, mtime, redaction posture) — so the default `staleTime: Infinity`
// is exactly right: scrubbing the playhead never refetches.

import { useQuery } from "@tanstack/react-query";
import { fetchSessionReplay, type SessionReplayResponse } from "../api/client";

export type UseSessionReplay = {
  data: SessionReplayResponse | undefined;
  loading: boolean;
  error: string | null;
};

/// Fetch one session's replay timeline. `artifactId` (optional) asks the
/// daemon for only the beats that resolved to that artifact — a genuinely
/// different window, hence its own cache entry.
export function useSessionReplay(
  sessionId: string | null | undefined,
  artifactId?: string | null,
): UseSessionReplay {
  const q = useQuery({
    queryKey: ["sessions", "replay", sessionId ?? "", artifactId ?? ""] as const,
    enabled: !!sessionId,
    queryFn: ({ signal }) =>
      fetchSessionReplay(
        sessionId as string,
        artifactId ? { artifact: artifactId } : {},
        signal,
      ),
  });
  return {
    data: q.data,
    loading: q.isPending && !!sessionId,
    error: q.error ? String(q.error) : null,
  };
}
