// CONVRAIL — the Conversation rail's read of `GET /api/sessions/{sid}/view
// ?fields=conversation`. Server state lives in TanStack (#23): finite-by-event,
// staleTime Infinity, NO polling; the bridge in api/queryClient.ts invalidates
// the key on session.state / session.captured. Only the projection is held;
// terminal bytes never enter the cache.
//
// The server is stateless: the queryFn sends the held `capture_gen` back as
// `conv_gen`, and an `unchanged:true` answer (no rows) keeps the rows we hold.

import { useQuery, useQueryClient, type QueryClient } from "@tanstack/react-query";
import { fetchConversation, type ConversationResult } from "../api/sessions";

export const conversationKey = (sid: string | undefined) => ["sessions", "conversation", sid] as const;

/** Merge an `unchanged:true` answer onto what the cache already holds. */
export function mergeUnchanged(held: ConversationResult | undefined, next: ConversationResult): ConversationResult {
  if (next.kind === "ok" && next.conversation.unchanged && held?.kind === "ok") return held;
  return next;
}

export function useConversation(sid: string | undefined) {
  const qc: QueryClient = useQueryClient();
  return useQuery<ConversationResult>({
    queryKey: conversationKey(sid),
    enabled: !!sid,
    staleTime: Infinity,
    queryFn: async ({ signal }) => {
      const held = qc.getQueryData<ConversationResult>(conversationKey(sid));
      const gen = held?.kind === "ok" ? held.conversation.capture_gen : undefined;
      const next = await fetchConversation(sid as string, { gen, signal });
      return mergeUnchanged(held, next);
    },
  });
}
