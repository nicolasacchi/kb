// LSC-4 — the live-sessions cockpit read hook: `GET /api/sessions/live-status`
// (LSC-2's cross-harness beat registry + Tier-0 capture-derived merge).
//
// Invariant #23's FIFTH documented exception (see queryClient.ts's ledger
// comment for the full rationale — this hook is named there too): a finite
// staleTime + a matching ~10s refetchInterval, because elapsed timers are a
// CLIENT concern (`useNowTick` ticks the "Xm ago" labels off `since_unix`;
// re-polling faster buys nothing) PLUS a targeted invalidation on the
// `session.state` SSE kind, wired through the EXISTING bridge
// (`queryClient.ts`'s `startSseInvalidationBridge`) so a real transition
// sharpens the view immediately rather than waiting out the poll. This hook
// opens NO stream of its own — invariant #24, one SSE connection per
// browser via the SharedWorker; every consumer (the /sessions Now band, the
// Header chip) shares this ONE cached query/poll.
//
// Mirrors `useSessionPresence` (useSessions.ts)'s shape — the nearest
// existing #23 exception — but adds the bridge wiring that hook explicitly
// opts out of (presence has no corresponding SSE event to react to;
// `session.state` does).

import { useEffect } from "react";
import { useQuery } from "@tanstack/react-query";
import { fetchLiveStatus, type LiveStatusRow } from "../api/sessions";
import { blockedCount } from "../lib/liveSessionLanes";
import { setAttentionCount } from "./useDocumentTitle";

/// Exported so the bridge (`queryClient.ts`) and any other consumer key off
/// the SAME query key rather than re-typing the literal.
export const LIVE_SESSIONS_KEY = ["sessions", "live"] as const;

const POLL_MS = 10_000;
const EMPTY: LiveStatusRow[] = [];

export function useLiveSessions(): {
  rows: LiveStatusRow[];
  loading: boolean;
} {
  const q = useQuery({
    queryKey: LIVE_SESSIONS_KEY,
    queryFn: ({ signal }) => fetchLiveStatus(signal),
    staleTime: POLL_MS,
    refetchInterval: POLL_MS,
  });
  const rows = q.data ?? EMPTY;
  // FUTERMATTN: the blocked count rides this ONE cached query into the tab
  // title and the app badge (the Header chip always mounts a consumer); no
  // second poll, no stream. Idempotent across consumers.
  const blocked = blockedCount(rows);
  useEffect(() => {
    if (q.isPending) return;
    setAttentionCount(blocked);
  }, [blocked, q.isPending]);
  return { rows, loading: q.isPending };
}
