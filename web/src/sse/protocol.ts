// Wire types shared by the SSE core, the tab-side facade, and (SW2) the
// SharedWorker transport. This module must stay context-agnostic: no DOM,
// no localStorage, no React — it is imported from both window and worker
// contexts.

/// Bumped when the TabToWorker/WorkerToTab shapes change incompatibly. A
/// tab whose `hello` carries a different version gets no `snapshot` reply
/// and falls back to the in-tab direct transport (handshake timeout).
export const SSE_PROTOCOL_V = 1;

/// Derived per-daemon scalars — everything the status pill needs, nothing
/// it doesn't. The reducer's raw state (the runs Set, the per-artifact
/// comment Map) stays inside the core; snapshots are cheap to structured-
/// clone across a MessagePort.
export type DaemonSnapshot = {
  url: string;
  connected: boolean;
  /// unix ms of the last frame from this daemon (any type), null before
  /// the first one.
  lastEventAt: number | null;
  inFlight: number;
  openErrors: number;
  /// Sum of latest open-counts across artifacts (comments.updated).
  openComments: number;
  /// runs.size — consumers only need emptiness to derive "indexing".
  activeRuns: number;
  /// CT — the last reconnect attempt looked auth-shaped, not daemon-down
  /// (a 401/403, a redirect to a different origin, or a 2xx whose body
  /// isn't actually an event stream — an expired forward-auth session).
  /// Optional: a pre-CT worker's snapshot simply won't carry it, and a
  /// pre-CT tab ignores it — additive, doesn't bump `SSE_PROTOCOL_V`.
  authSuspect?: boolean;
};

/// One SSE frame, normalised. `payload` is the daemon envelope's inner
/// `payload` field; synthetic frames (`lag`, `gap`) aren't enveloped, so
/// their parsed body rides `payload` directly. `id` mirrors EventSource's
/// sticky lastEventId semantics (null until the first `id:` line).
export type EventEnvelope = {
  daemonUrl: string;
  type: string;
  payload: Record<string, unknown>;
  id: string | null;
  /// Receive time (unix ms) in whichever context ran the core.
  at: number;
};

export type TabToWorker =
  /// First message on a fresh port. `daemons` + `cursors` seed the worker
  /// ONLY if it is uninitialized — a long-lived stale tab must not revert
  /// a newer `set-daemons`.
  | {
      kind: "hello";
      v: number;
      buildSha: string;
      daemons: string[];
      cursors: Record<string, string>;
    }
  /// DaemonsManager save — always wins; worker reconciles and broadcasts
  /// a fresh `snapshot` to every port.
  | { kind: "set-daemons"; daemons: string[] }
  /// pagehide(!persisted) — belt-and-braces port cleanup beside the
  /// MessagePort `close` event.
  | { kind: "bye" };

export type WorkerToTab =
  /// Reply to `hello` / broadcast after `set-daemons`. Always the FIRST
  /// message a port receives (MessagePort delivery is FIFO), so a late-
  /// joining tab renders correct status before any incremental frame.
  | { kind: "snapshot"; v: number; daemons: DaemonSnapshot[] }
  | { kind: "status"; daemon: DaemonSnapshot }
  | { kind: "event"; event: EventEnvelope }
  /// Server `gap` frame — the events in between are unknowable; tabs
  /// invalidate their query caches wholesale.
  | { kind: "resync"; daemonUrl: string };
