// A4 — the `kb-session-card/1` read for `/sessions/:sid`. The bridge owns
// freshness (invariant #23): `staleTime: Infinity`, NO `refetchInterval`
// (this is not a new #23 exception). The key rides the `["sessions"]`
// prefix, so `useSessions`' own `session.captured`/`session.deleted`
// subscription already invalidates it; the bridge adds the
// `session.state` and `slate.updated` kinds (see `api/queryClient.ts`).

import { useQuery } from "@tanstack/react-query";
import { fetchSessionCard, type SessionCard } from "../api/sessions";

export const SESSION_CARD_KEY = (sid: string) =>
  ["sessions", "card", sid] as const;

export function useSessionCard(sid: string): {
  card: SessionCard | null;
  loading: boolean;
  error: string | null;
} {
  const q = useQuery({
    queryKey: SESSION_CARD_KEY(sid),
    queryFn: ({ signal }) => fetchSessionCard(sid, signal),
    staleTime: Infinity,
  });
  return {
    card: q.data ?? null,
    loading: q.isPending,
    error: q.error ? String(q.error) : null,
  };
}
