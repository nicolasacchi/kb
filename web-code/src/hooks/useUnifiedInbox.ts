// S2-A — kb-code v6.0 "One Inbox" (design-s2.md §S2-A). `GET /api/inbox`
// spans every configured repo (plus kb, when reachable) rather than one
// repo's own working tree/review state, so this hook deliberately does NOT
// key under the `["reviews", repo]` / `["annotations", repo, path]` prefixes
// the rest of this app's SSE bridge (`api/queryClient.ts`) already
// understands — a repo-scoped prefix invalidation would never reach it.
//
// `staleTime: 30s` + a manual Refresh button is the documented no-SSE-tie
// exception (root CLAUDE.md invariant #23's third kind — "finite staleTime
// + manual refresh", same shape `hooks/useDocLens.ts` already uses for the
// analogous "depends on kb's own content, no matching kb-code SSE event"
// case). ON TOP of that floor, `api/queryClient.ts`'s `review.changed`
// handler ALSO prefix-invalidates this query's key directly (a <5-line
// addition — the smaller of the two diffs design-s2.md offers, since the
// review lane is one third of this page and review mutations are by far
// the highest-frequency source of "this page is now stale"). The
// annotations/kb lanes have no equivalent kb-code event to tie into (no
// `annotation.changed` fan-out reaches a repo-less query key without
// widening that handler's contract) — 30s staleTime covers them.

import { useQuery } from "@tanstack/react-query";
import { fetchUnifiedInbox } from "../api/client";

export function unifiedInboxKey() {
  return ["inbox", "unified"] as const;
}

const UNIFIED_INBOX_STALE_MS = 30_000;

export function useUnifiedInbox() {
  return useQuery({
    queryKey: unifiedInboxKey(),
    queryFn: fetchUnifiedInbox,
    staleTime: UNIFIED_INBOX_STALE_MS,
  });
}
