import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { sse } from "../api/sse";

// Filter-UI vocabulary for the Live tab's kind checklist — NO LONGER
// load-bearing for reception. The SSE core delivers every frame
// generically (fetch-parsed, no per-type listeners), so event kinds
// missing from this list still arrive and render; they just don't get a
// dedicated filter checkbox until added here.
export const KNOWN_EVENT_KINDS = [
  "index.start",
  "index.file",
  "index.embedding",
  "index.complete",
  "artifact.indexed",
  "artifact.removed",
  "watch.create",
  "watch.modify",
  "watch.delete",
  "watcher.lagged",
  "query",
  "error",
  "error.dismissed",
  "error.fixed",
  "source.paused",
  "source.resumed",
  "atlas.recompute.start",
  "atlas.recompute.complete",
  "atlas.recluster.start",
  "atlas.recluster.complete",
  "comment.anchor_stale",
  "comment.anchor_resolved",
  "comments.updated",
  "history.recorded",
  "reconcile.complete",
  "memory.ingested",
  "memory.stale",
  "memory.resolved",
  "memory.forgotten",
  "metrics.tick",
  // Synthetic frames the server emits to surface stream-health issues.
  "lag",
  "gap",
] as const;

export type EventKind = (typeof KNOWN_EVENT_KINDS)[number];

export type LogEntry = {
  /// Monotonic local sequence — never gaps, used as a stable React key.
  seq: number;
  kind: string;
  payload: Record<string, unknown>;
  /// Daemon URL the event arrived from.
  daemonUrl: string;
  /// wall-clock ms when the frame was received.
  at: number;
};

const RING_CAPACITY = 500;

// useEventLog — the Live dashboard tab's firehose view, fed from the
// shared SSE dispatch (sse.subscribeAll). Historically this opened its
// own unfiltered EventSource per daemon; those sockets counted against
// the browser's 6-per-host connection pool and are gone — the single
// shared stream already carries everything.
//
// `enabled` gates the subscription: false → no listener, no buffer
// growth. `kindFilter` is the per-render allow-list (empty = show all);
// applied client-side so toggling a filter loses nothing.
//
// Returns: ring buffer (newest at end), clear. Buffer is mutable
// through a ref + setState to dodge a re-render per frame — the
// rendered slice is the *filtered* view, throttled by React's batching.
export function useEventLog(opts: {
  enabled: boolean;
  paused: boolean;
  kindFilter: Set<string>;
}): { entries: LogEntry[]; clear: () => void } {
  const { enabled, paused, kindFilter } = opts;
  const seqRef = useRef(0);
  const bufRef = useRef<LogEntry[]>([]);
  const [, setBumper] = useState(0);
  const pausedRef = useRef(paused);
  useEffect(() => {
    pausedRef.current = paused;
  }, [paused]);

  useEffect(() => {
    if (!enabled) return;
    return sse.subscribeAll((e) => {
      if (pausedRef.current) return;
      const entry: LogEntry = {
        seq: ++seqRef.current,
        kind: e.type,
        payload: e.payload,
        daemonUrl: e.daemonUrl,
        at: e.at,
      };
      const buf = bufRef.current;
      buf.push(entry);
      if (buf.length > RING_CAPACITY) buf.splice(0, buf.length - RING_CAPACITY);
      // Bump in a microtask so a burst of events collapses to one
      // re-render per microtask (and React batches further).
      setBumper((n) => n + 1);
    });
  }, [enabled]);

  const clear = useCallback(() => {
    bufRef.current = [];
    setBumper((n) => n + 1);
  }, []);

  const entries = useMemo(() => {
    if (kindFilter.size === 0) return bufRef.current.slice();
    return bufRef.current.filter((e) => kindFilter.has(e.kind));
    // bufRef contents change without setBumper firing reliably for
    // memoised consumers — re-eval whenever the bumper changes. Done
    // implicitly because setBumper triggers the parent re-render that
    // re-evaluates this hook.
  }, [kindFilter, bufRef.current.length, /* trigger on bumper */ seqRef.current]);

  return { entries, clear };
}
