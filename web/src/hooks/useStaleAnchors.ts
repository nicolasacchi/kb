import { useCallback, useEffect, useState } from "react";
import { fetchStaleAnchors } from "../api/client";
import { sse } from "../api/sse";

// One row in the stale-anchors dashboard. Keyed by
// `${kb}:${artifact_id}:${comment_id}` — the indexer's anchor
// tracker uses the same composite key.
export type StaleAnchor = {
  kb: string;
  artifactId: string;
  commentId: string;
  /// "exact" / "fuzzy" / "stale" — the kind from the indexer's
  /// fuzzy_resolve_anchor pass that produced this event.
  anchorKind: string;
  /// 0..1 similarity score from the fuzzy resolver. 0 means no match
  /// at all; 1 means perfect. Stale anchors are by definition
  /// below the daemon's accept threshold.
  fuzzyScore: number;
  /// Track U — source-relative path of the artifact, for the
  /// `/a/<kb>/<path>` deep-link. Absent when the artifact has left lance
  /// (cold load) or an older daemon's event omitted it.
  sourceRelative?: string;
  /// Wall-clock time we first saw this stale event (this session).
  firstSeen: number;
};

// useStaleAnchors — subscribes to comment.anchor_stale +
// comment.anchor_resolved across all configured daemons and exposes
// the current set of stale anchors. Cold-loads from
// `GET /api/anchors/stale` on mount so the dashboard survives a
// page reload (Q1) — the endpoint reads each kb's persisted
// `.anchors-stale.json` sidecar. Live SSE events take over from there;
// they're the source of truth for `anchorKind` / `fuzzyScore`, which
// the cold-load shape doesn't include (those are session-scoped).
export function useStaleAnchors(): StaleAnchor[] {
  const [stale, setStale] = useState<Map<string, StaleAnchor>>(new Map());

  // Load (or reload) the set from the persisted `.anchors-stale.json`
  // sidecar via `GET /api/anchors/stale`. `replace` REPLACES the Map (used on
  // an SSE resync, to drop phantom pre-gap entries the sidecar no longer
  // lists); otherwise it merges without clobbering live-event metadata
  // (anchorKind/fuzzyScore), which the cold-load shape doesn't carry.
  const coldLoad = useCallback((signal: AbortSignal, replace: boolean) => {
    fetchStaleAnchors(signal)
      .then((resp) => {
        // Merge of nothing into a non-empty map is a no-op — skip the
        // setState. On a resync we must still run (to clear), so don't skip.
        if (resp.anchors.length === 0 && !replace) return;
        const now = Date.now();
        setStale((prev) => {
          const next = replace
            ? new Map<string, StaleAnchor>()
            : new Map(prev);
          for (const a of resp.anchors) {
            const key = `${a.kb}:${a.artifact_id}:${a.comment_id}`;
            // Don't overwrite a live-event entry — the live event
            // carries richer metadata (anchorKind, fuzzyScore).
            if (next.has(key)) continue;
            next.set(key, {
              kb: a.kb,
              artifactId: a.artifact_id,
              commentId: a.comment_id,
              // #4 — server now ships the v3 sidecar metadata; fall
              // back to the pre-v3 defaults only if the field is
              // somehow missing (older daemon behind the SPA).
              anchorKind: a.anchor_kind || "stale",
              fuzzyScore: typeof a.fuzzy_score === "number" ? a.fuzzy_score : 0,
              sourceRelative: a.source_relative ?? undefined,
              firstSeen: now,
            });
          }
          return next;
        });
      })
      .catch(() => {
        // Daemon unreachable on cold load → keep the current map (session-only
        // mode, the pre-Q1 behavior). Live events still populate it.
      });
  }, []);

  // Cold load — runs once on mount. AbortController guards the
  // unmount-during-fetch case so React 18's StrictMode double-mount
  // doesn't leak in dev.
  useEffect(() => {
    const ac = new AbortController();
    coldLoad(ac.signal, false);
    return () => ac.abort();
  }, [coldLoad]);

  useEffect(() => {
    // comment.anchor_stale — add to the set.
    const offStale = sse.subscribeEvent("comment.anchor_stale", (payload) => {
      const kb = stringField(payload.kb);
      const artifactId = stringField(payload.artifact_id);
      const commentId = stringField(payload.comment_id) ?? stringField(payload.id);
      if (!kb || !artifactId || !commentId) return;
      const key = `${kb}:${artifactId}:${commentId}`;
      const anchorKind = stringField(payload.anchor_kind) ?? "stale";
      const fuzzyScore = numberField(payload.fuzzy_score) ?? 0;
      const sourceRelative = stringField(payload.source_relative) ?? undefined;
      setStale((prev) => {
        if (prev.has(key)) return prev;
        const next = new Map(prev);
        next.set(key, {
          kb,
          artifactId,
          commentId,
          anchorKind,
          fuzzyScore,
          sourceRelative,
          firstSeen: Date.now(),
        });
        return next;
      });
    });

    // comment.anchor_resolved — drop from the set.
    const offResolved = sse.subscribeEvent("comment.anchor_resolved", (payload) => {
      const kb = stringField(payload.kb);
      const artifactId = stringField(payload.artifact_id);
      const commentId = stringField(payload.comment_id);
      if (!kb || !artifactId || !commentId) return;
      const key = `${kb}:${artifactId}:${commentId}`;
      setStale((prev) => {
        if (!prev.has(key)) return prev;
        const next = new Map(prev);
        next.delete(key);
        return next;
      });
    });

    // On an SSE gap/resync the worker's cursor jumped — anchor_stale /
    // anchor_resolved events during the disconnect were missed, so the Map can
    // hold phantom (since-resolved) entries. Re-load from the persisted sidecar
    // and REPLACE the Map, dropping anything it no longer lists (invariant #23
    // exception: this store is event-sourced, so it owns its own resync).
    let resyncAc: AbortController | null = null;
    const offResync = sse.onResync(() => {
      resyncAc?.abort();
      resyncAc = new AbortController();
      coldLoad(resyncAc.signal, true);
    });

    return () => {
      offStale();
      offResolved();
      offResync();
      resyncAc?.abort();
    };
  }, [coldLoad]);

  // Stable order: oldest-first by firstSeen. Newly-stale anchors
  // appear at the bottom so the operator's eye stays anchored on
  // the things they've been ignoring longest.
  return Array.from(stale.values()).sort((a, b) => a.firstSeen - b.firstSeen);
}

function stringField(v: unknown): string | null {
  return typeof v === "string" && v.length > 0 ? v : null;
}

function numberField(v: unknown): number | null {
  return typeof v === "number" && Number.isFinite(v) ? v : null;
}
