import { useCallback } from "react";
import { useQuery, useQueryClient } from "@tanstack/react-query";
import { fetchInbox, type InboxItem, type InboxResponse } from "../api/inbox";
import { excludeArtifact, resolveComment as apiResolveComment } from "../api/client";

// Z4 — fleet-wide open-comments inbox, invariant #23 shaped: ONE shared
// ["inbox"] cache entry serves both the Header badge (`totalOpen`) and the
// /inbox route (`items`). The SSE bridge (queryClient.ts) invalidates it on
// `comments.updated` + gap resync, so this hook carries zero subscribe
// plumbing — it's a plain useQuery over the SSE-driven cache.
//
// W1.mobile — the per-item triage actions (resolve/archive) live here too,
// not in the route: they're just mutations against the SAME cache entry the
// query above reads, so patching it optimistically is the natural home
// (same shape as useReview's `commit` — patch first, and on a rejection
// invalidate to resync rather than hand-roll a snapshot/revert). `resolve`
// reuses the exact api fn CommentsPanel's resolve button calls
// (`resolveComment` from api/client.ts, driven there via useReview);
// `archive` reuses the existing per-file exclusion (`excludeArtifact`,
// Card.tsx's/PreviewInspector's "exclude" action) — reversible from
// Settings → Excluded. Neither mutation bumps the index generation (#15
// doesn't apply — comments/exclusions aren't row-set data) and neither is a
// new SSE-shaped subscription (#23) — they're plain awaited calls.

const KEY = ["inbox"] as const;
const EMPTY: InboxItem[] = [];

export type UseInboxResult = {
  items: InboxItem[];
  /// Fleet-wide count of open comments (may exceed `items.length` when the
  /// server capped the page). Drives the Header badge.
  totalOpen: number;
  loading: boolean;
  error: string | null;
  /// Resolve one open comment. Optimistically drops its row (and decrements
  /// `totalOpen`); on failure, invalidates `["inbox"]` to resync and
  /// rethrows so the caller can toast.err (#32).
  resolveItem: (kb: string, artifactId: string, commentId: string) => Promise<void>;
  /// Archive the WHOLE artifact (every open comment on it) via the existing
  /// per-file exclusion. Optimistically drops every row sharing
  /// `(kb, artifactId)`; on failure, invalidates to resync and rethrows.
  archiveArtifact: (
    kb: string,
    artifactId: string,
    sourceRelative: string,
  ) => Promise<void>;
};

export function useInbox(): UseInboxResult {
  const queryClient = useQueryClient();
  const q = useQuery({
    queryKey: KEY,
    queryFn: ({ signal }) => fetchInbox({}, signal),
  });

  const resolveItem = useCallback(
    async (kb: string, artifactId: string, commentId: string) => {
      queryClient.setQueryData<InboxResponse>(KEY, (prev) =>
        prev
          ? {
              items: prev.items.filter((i) => i.comment_id !== commentId),
              total_open: Math.max(0, prev.total_open - 1),
            }
          : prev,
      );
      try {
        await apiResolveComment(kb, artifactId, commentId);
      } catch (e) {
        void queryClient.invalidateQueries({ queryKey: KEY });
        throw e;
      }
    },
    [queryClient],
  );

  const archiveArtifact = useCallback(
    async (kb: string, artifactId: string, sourceRelative: string) => {
      queryClient.setQueryData<InboxResponse>(KEY, (prev) => {
        if (!prev) return prev;
        const dropped = prev.items.filter(
          (i) => i.kb === kb && i.artifact_id === artifactId,
        ).length;
        return {
          items: prev.items.filter(
            (i) => !(i.kb === kb && i.artifact_id === artifactId),
          ),
          total_open: Math.max(0, prev.total_open - dropped),
        };
      });
      try {
        await excludeArtifact(kb, sourceRelative);
      } catch (e) {
        void queryClient.invalidateQueries({ queryKey: KEY });
        throw e;
      }
    },
    [queryClient],
  );

  return {
    items: q.data?.items ?? EMPTY,
    totalOpen: q.data?.total_open ?? 0,
    loading: q.isPending,
    error: q.error ? String(q.error) : null,
    resolveItem,
    archiveArtifact,
  };
}
