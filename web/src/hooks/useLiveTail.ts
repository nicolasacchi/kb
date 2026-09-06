// W7 (sessions-rethink R15/LF-2) — the Tier-1 live-tail poll loop against
// `GET /api/sessions/{sid}/live?from=` (LF-3b). Deliberately NOT a TanStack
// Query cache entry: the design's own reasoning (LF-2/#23) is that this
// data is cumulative-append, ephemeral, never invalidated by anything else
// in the app, and never shared across components — putting it in the
// shared cache would create a SECOND home for transcript state that could
// drift from what this hook's own accumulation ref holds. State lives in a
// plain ref + `useState` mirror (for re-renders), dies with the component.
//
// Polls every `POLL_MS` while `enabled` AND the tab is visible (a
// `visibilitychange` listener pauses the loop outright while hidden, rather
// than fetching-and-discarding — no network activity happens in a
// backgrounded tab). Stops entirely on unmount, `enabled` flipping false,
// or the daemon reporting `ended` (a capture superseded the live file and
// it's no longer being written — the caller should hand off to the
// captured render at that point, see `ArtifactPane`'s handoff effect).

import { useEffect, useRef, useState } from "react";
import { fetchLiveDelta } from "../api/sessions";
import type { Turn } from "../api/generated/Turn";
import type { TaskBoardRow } from "../api/generated/TaskBoardRow";

/// LF-2 — the panel's own poll cadence (distinct from the presence probe's
/// 30s — this is the "actually watching a live session" cadence).
export const LIVE_TAIL_POLL_MS = 3000;
/// LF-2 — "bounded to the last LIVE_TAIL_MAX_TURNS (200) turns, older
/// entries dropping off the top".
export const LIVE_TAIL_MAX_TURNS = 200;

// The wire `ViewEvent` is a `#[serde(tag = "kind")]` union of `TurnClosed`
// (a flattened `Turn`) and `TaskUpdated` (a flattened `TaskBoardRow`).
// Narrowed locally rather than importing the generated union type directly
// — the runtime `kind` tag is the only thing this hook actually depends on;
// the exact TS shape ts-rs emits for a tagged NEWTYPE-variant union is an
// implementation detail this hook doesn't need to name.
type WireViewEvent =
  | ({ kind: "TurnClosed" } & Turn)
  | ({ kind: "TaskUpdated" } & TaskBoardRow);

export interface LiveTailState {
  /// Accumulated CLOSED turns, oldest first, capped at
  /// `LIVE_TAIL_MAX_TURNS` (a "full transcript below ↑" divider is the
  /// panel's job to render when truncated — see `truncatedAtTop`).
  turns: Turn[];
  /// True once accumulated turns have been dropped off the top.
  truncatedAtTop: boolean;
  /// Latest known status of each task id touched this follow session
  /// (LF-4: "a task-board delta without waiting for the owning turn to
  /// close").
  taskUpdates: TaskBoardRow[];
  /// `mtime(file) < live_window_secs` as of the last successful poll.
  live: boolean;
  /// Advisory — see `LiveDeltaResponse.ended`'s own doc. The real stop
  /// signal is `session.captured`, which the caller (ArtifactPane) listens
  /// for independently; this hook just stops polling once it sees `ended`.
  ended: boolean;
  parseFailures: number;
  /// Non-null after a fetch failure; cleared on the next successful poll.
  /// The hook keeps retrying (backed off) rather than giving up — a
  /// transient daemon hiccup shouldn't permanently end a follow session.
  error: string | null;
  /// Whether the initial bootstrap response has landed yet (distinguishes
  /// "no turns because nothing has closed yet" from "still loading").
  ready: boolean;
}

const INITIAL: LiveTailState = {
  turns: [],
  truncatedAtTop: false,
  taskUpdates: [],
  live: false,
  ended: false,
  parseFailures: 0,
  error: null,
  ready: false,
};

export function useLiveTail(
  sessionId: string | null,
  enabled: boolean,
): LiveTailState {
  const [state, setState] = useState<LiveTailState>(INITIAL);
  const fromRef = useRef(0);
  const turnsRef = useRef<Turn[]>([]);
  const truncatedRef = useRef(false);
  const taskRef = useRef<Map<string, TaskBoardRow>>(new Map());
  const seenTurnIds = useRef<Set<string>>(new Set());

  // A fresh session (or the same session re-entering follow mode after
  // leaving it) starts a fresh accumulation — never carries stale turns
  // from a previously-followed session into the new one.
  useEffect(() => {
    fromRef.current = 0;
    turnsRef.current = [];
    truncatedRef.current = false;
    taskRef.current = new Map();
    seenTurnIds.current = new Set();
    setState(INITIAL);
  }, [sessionId]);

  useEffect(() => {
    if (!enabled || !sessionId) return;
    let alive = true;
    let timer: ReturnType<typeof setTimeout> | null = null;
    let backoff = LIVE_TAIL_POLL_MS;

    function scheduleNext(ms: number) {
      if (!alive || document.visibilityState !== "visible") return;
      timer = setTimeout(poll, ms);
    }

    async function poll() {
      if (!alive || document.visibilityState !== "visible") return;
      try {
        const delta = await fetchLiveDelta(
          sessionId as string,
          fromRef.current,
          false,
        );
        if (!alive) return;
        backoff = LIVE_TAIL_POLL_MS;
        fromRef.current = delta.next_from;
        for (const raw of delta.events ?? []) {
          const ev = raw as unknown as WireViewEvent;
          if (ev.kind === "TurnClosed") {
            if (!seenTurnIds.current.has(ev.id)) {
              seenTurnIds.current.add(ev.id);
              const next = [...turnsRef.current, ev as unknown as Turn];
              if (next.length > LIVE_TAIL_MAX_TURNS) {
                truncatedRef.current = true;
                turnsRef.current = next.slice(next.length - LIVE_TAIL_MAX_TURNS);
              } else {
                turnsRef.current = next;
              }
            }
          } else if (ev.kind === "TaskUpdated") {
            taskRef.current.set(ev.id, ev as unknown as TaskBoardRow);
          }
        }
        setState({
          turns: turnsRef.current,
          truncatedAtTop: truncatedRef.current,
          taskUpdates: Array.from(taskRef.current.values()),
          live: delta.live,
          ended: delta.ended,
          parseFailures: delta.parse_failures,
          error: null,
          ready: true,
        });
        if (!delta.ended) {
          scheduleNext(LIVE_TAIL_POLL_MS);
        }
        // `ended` — stop polling; the caller's own `session.captured`
        // listener owns the handoff from here.
      } catch (e) {
        if (!alive) return;
        setState((s) => ({
          ...s,
          error: e instanceof Error ? e.message : String(e),
          ready: true,
        }));
        backoff = Math.min(backoff * 2, LIVE_TAIL_POLL_MS * 8);
        scheduleNext(backoff);
      }
    }

    function onVisibilityChange() {
      if (document.visibilityState === "visible") {
        // Resume immediately rather than waiting out whatever was left of
        // the paused interval — the point of pausing was "no fetches while
        // hidden", not "stay stale after coming back".
        if (timer) {
          clearTimeout(timer);
          timer = null;
        }
        void poll();
      } else if (timer) {
        clearTimeout(timer);
        timer = null;
      }
    }

    document.addEventListener("visibilitychange", onVisibilityChange);
    void poll();
    return () => {
      alive = false;
      document.removeEventListener("visibilitychange", onVisibilityChange);
      if (timer) clearTimeout(timer);
    };
  }, [enabled, sessionId]);

  return state;
}
