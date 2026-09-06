import { useCallback } from "react";
import { useQuery, useQueryClient } from "@tanstack/react-query";
import {
  fetchMemoryCommittedIn,
  fetchMemoryLineage,
  fetchMemoryRecalledBy,
  fetchRecall,
  type MemoryCommittedInRow,
  type MemoryRecalledByRow,
  type MemoryScope,
  type RecallHit,
} from "../api/client";
import { fetchSessionMemories } from "../api/sessions";

// TQ3: recall results live under ["memories", params]; the SSE bridge
// (api/queryClient.ts) invalidates the ["memories"] prefix on every
// memory.* event (ingested/stale/resolved/forgotten/linked/unlinked),
// on session.captured (the session lens), and on gap resync — the
// per-hook subscription lists this file used to carry are gone.

export type UseMemories = {
  hits: RecallHit[];
  loading: boolean;
  error: string | null;
  refresh: () => void;
};

const EMPTY: RecallHit[] = [];

/// Cross-corpus memory list for the /memory view.
///
/// Three modes:
/// - **default** (`sessionId` absent): /api/memory/recall, scope/q honoured.
/// - **session lens** (`sessionId` present): /api/sessions/{sid}/memories,
///   scope+q ignored — the view shows only the memories that originated
///   in that Claude Code conversation.
/// - **per-kb filter** (`forKb` present): /api/memory/recall?for_kb=...,
///   only memories whose V0010 link set contains `*` (global) or `forKb`.
///   Compatible with scope/q.
export function useMemories(
  scope: MemoryScope,
  q: string,
  sessionId?: string | null,
  forKb?: string | null,
): UseMemories {
  const queryClient = useQueryClient();
  const query = useQuery({
    queryKey: [
      "memories",
      {
        scope,
        q: q || undefined,
        sessionId: sessionId ?? undefined,
        forKb: forKb ?? undefined,
      },
    ] as const,
    queryFn: async ({ signal }) => {
      if (sessionId) {
        const memories = await fetchSessionMemories(sessionId, signal);
        return memories.map(
          (m): RecallHit => ({
            id: m.id,
            kb: m.kb,
            title: m.title,
            path: m.path,
            source_relative: m.source_relative,
            score: 0,
            salience: m.kb_salience ?? 0.5,
            pinned: false,
            // The session endpoint doesn't carry link metadata; the
            // lens renders these rows without link chips.
            global: false,
            linked_kbs: [],
            // MI-W1.3 — the session-memories endpoint carries no recall
            // ledger stats; display-only enrichment, never scoring
            // input, so the honest default is "never recalled".
            recall_count: 0,
            // CT-C5 — same reasoning: no ledger data from this endpoint.
            recall_used_count: 0,
            last_recalled_at: undefined,
            // MI-W4.2a — same reasoning: no per-week histogram from this
            // endpoint, so the row sparkline (which needs decay_k/age_days
            // anyway, also absent here) simply has nothing to show.
            recall_weekly: [],
          }),
        );
      }
      const r = await fetchRecall(
        {
          q: q || undefined,
          scope,
          limit: 100,
          forKb: forKb ?? undefined,
          // MI-W4.2a — the /memory view is the ONE caller that wants the
          // per-week injection histogram (the row sparkline); every other
          // `fetchRecall` call site (the kb-recall hook, Cmdk, related-
          // memories panel) leaves it off.
          withWeekly: true,
        },
        signal,
      );
      return r.hits;
    },
  });

  const refresh = useCallback(
    () => void queryClient.invalidateQueries({ queryKey: ["memories"] }),
    [queryClient],
  );

  return {
    hits: query.data ?? EMPTY,
    loading: query.isPending,
    error: query.error ? String(query.error) : null,
    refresh,
  };
}

const EMPTY_RECALLED_BY: MemoryRecalledByRow[] = [];

export type UseMemoryRecalledBy = {
  rows: MemoryRecalledByRow[];
  loading: boolean;
  error: string | null;
};

/// CT-B2 — `GET /api/kb/{kb}/memories/{id}/recalled-by`'s SPA lens: every
/// session that recalled this ONE memory, for the memory-row provenance
/// modal's "Recall history" section. Nested under the SAME `["memories",
/// …]` query-key prefix `useMemories`/`useRelatedMemories` use, so the SSE
/// bridge's existing `["memories"]`-prefix invalidation (memory.* events)
/// covers it too — no new subscription wiring.
export function useMemoryRecalledBy(
  kb: string | null,
  id: string | null,
): UseMemoryRecalledBy {
  const q = useQuery({
    queryKey: ["memories", "recalled-by", kb ?? "", id ?? ""],
    enabled: !!kb && !!id,
    queryFn: ({ signal }) => fetchMemoryRecalledBy(kb as string, id as string, signal),
    staleTime: Infinity,
  });
  return {
    rows: q.data?.rows ?? EMPTY_RECALLED_BY,
    loading: q.isPending && !!kb && !!id,
    error: q.error ? String(q.error) : null,
  };
}

const EMPTY_COMMITTED_IN: MemoryCommittedInRow[] = [];

export type UseMemoryCommittedIn = {
  rows: MemoryCommittedInRow[];
  loading: boolean;
  error: string | null;
};

/// CT-F1 — `GET /api/kb/{kb}/memories/{id}/commits`'s SPA lens: every
/// commit that CITED this memory by id (a `Kb-Memory:` trailer), for the
/// dossier's exact-id section. Keyed under the SAME `["memories", …]`
/// prefix as `useMemoryRecalledBy`, so the SSE bridge's existing
/// `["memories"]`-prefix invalidation covers it — no new wiring (#23).
///
/// An empty `rows` is a NON-SIGNAL (the trailer is opt-in per repo,
/// default off); the renderer must never present it as "nothing used this
/// memory".
export function useMemoryCommittedIn(
  kb: string | null,
  id: string | null,
): UseMemoryCommittedIn {
  const q = useQuery({
    queryKey: ["memories", "committed-in", kb ?? "", id ?? ""],
    enabled: !!kb && !!id,
    queryFn: ({ signal }) => fetchMemoryCommittedIn(kb as string, id as string, signal),
    staleTime: Infinity,
  });
  return {
    rows: q.data?.rows ?? EMPTY_COMMITTED_IN,
    loading: q.isPending && !!kb && !!id,
    error: q.error ? String(q.error) : null,
  };
}

/// CT-B6 — the supersede chain for the dossier's Currency section
/// (`GET /api/kb/{kb}/memories/{id}/lineage`, the SAME endpoint
/// `LineageViewer` queries inline). Keyed under the `["memories", …]`
/// prefix — like `useMemoryRecalledBy` above — so the SSE bridge's
/// memory.* burst-gate invalidation covers a supersede landing while the
/// dossier is open (LineageViewer's own `["memory-lineage", kb, id]` key
/// predates that pattern and is a separate cache entry).
export function useMemoryLineage(kb: string | null, id: string | null, enabled = true) {
  return useQuery({
    queryKey: ["memories", "lineage", kb ?? "", id ?? ""],
    enabled: enabled && !!kb && !!id,
    queryFn: ({ signal }) => fetchMemoryLineage(kb as string, id as string, signal),
    staleTime: Infinity,
  });
}
