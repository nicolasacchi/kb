import { useCallback } from "react";
import { useQuery, useQueryClient } from "@tanstack/react-query";
import {
  fetchAnchors,
  pinAnchor,
  unpinAnchor,
  type CorkboardEntry,
} from "../api/client";

// v0.10 K3 — cross-kb anchor corkboard, live-synced. TQ3: one shared
// ["anchors"] cache entry across every consumer; the SSE bridge
// invalidates it on anchor.added/removed and gap resync (which also
// reconciles the optimistic entries below with the lance projection's
// title/folder/source_relative join).

const KEY = ["anchors"] as const;
const EMPTY: CorkboardEntry[] = [];

export type UseAnchorsResult = {
  anchors: CorkboardEntry[];
  count: number;
  loading: boolean;
  error: string | null;
  /// True iff the (kb, artifact_id) pair is on the corkboard right now.
  /// Cheap O(N) — the list never grows past the user's curated set
  /// (single-digits to low hundreds in steady state).
  isAnchored: (kb: string, artifactId: string) => boolean;
  pin: (kb: string, artifactId: string) => Promise<void>;
  unpin: (kb: string, artifactId: string) => Promise<void>;
  toggle: (kb: string, artifactId: string) => Promise<void>;
  refresh: () => void;
};

export function useAnchors(): UseAnchorsResult {
  const queryClient = useQueryClient();
  const q = useQuery({
    queryKey: KEY,
    queryFn: ({ signal }) => fetchAnchors(signal).then((r) => r.anchors),
  });
  const anchors = q.data ?? EMPTY;

  const isAnchored = useCallback(
    (kb: string, artifactId: string) =>
      anchors.some((a) => a.kb === kb && a.artifact_id === artifactId),
    [anchors],
  );

  const pin = useCallback(
    async (kb: string, artifactId: string) => {
      // Optimistic insert at the head; the SSE-driven refetch reconciles
      // with the lance projection (title/folder/source_relative).
      const before = queryClient.getQueryData<CorkboardEntry[]>(KEY);
      queryClient.setQueryData<CorkboardEntry[]>(KEY, (prev = EMPTY) =>
        prev.some((a) => a.kb === kb && a.artifact_id === artifactId)
          ? prev
          : [
              {
                kb,
                artifact_id: artifactId,
                created_at: Math.floor(Date.now() / 1000),
              },
              ...prev,
            ],
      );
      try {
        await pinAnchor(kb, artifactId);
      } catch (e) {
        // Roll back; consumers fire-and-forget these, so don't rethrow.
        queryClient.setQueryData(KEY, before);
        console.warn("[anchors] pin failed", e);
      }
    },
    [queryClient],
  );

  const unpin = useCallback(
    async (kb: string, artifactId: string) => {
      const before = queryClient.getQueryData<CorkboardEntry[]>(KEY);
      queryClient.setQueryData<CorkboardEntry[]>(KEY, (prev = EMPTY) =>
        prev.filter((a) => !(a.kb === kb && a.artifact_id === artifactId)),
      );
      try {
        await unpinAnchor(kb, artifactId);
      } catch (e) {
        // Roll back; consumers fire-and-forget these, so don't rethrow.
        queryClient.setQueryData(KEY, before);
        console.warn("[anchors] unpin failed", e);
      }
    },
    [queryClient],
  );

  const toggle = useCallback(
    async (kb: string, artifactId: string) => {
      if (isAnchored(kb, artifactId)) await unpin(kb, artifactId);
      else await pin(kb, artifactId);
    },
    [isAnchored, pin, unpin],
  );

  const refresh = useCallback(
    () => void queryClient.invalidateQueries({ queryKey: KEY }),
    [queryClient],
  );

  return {
    anchors,
    count: anchors.length,
    loading: q.isPending,
    error: q.error ? String(q.error) : null,
    isAnchored,
    pin,
    unpin,
    toggle,
    refresh,
  };
}
