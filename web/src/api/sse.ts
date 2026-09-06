// Tab-side SSE facade. Owns the listener registries (aggregated status,
// per-event-type, firehose, resync), the daemon-list persistence
// (localStorage `kb:daemons`), and the AggregatedStatus computation.
// The actual connections live behind the transport (../sse/transport.ts):
// a SharedWorker hosting the context-agnostic SseCore — ONE unfiltered
// `/api/events` stream per daemon for ALL same-origin tabs (invariant
// #24) — or the identical core inline in this tab when SharedWorker is
// unavailable / the kill-switch is set.
//
// Persisted to localStorage:
//   kb:daemons          — JSON array of daemon URLs
//   kb:lid:<url>        — resume cursor per daemon (the worker advances,
//                         tabs mirror; direct mode writes it directly)
//   kb:sse:transport    — "direct" forces the in-tab fallback transport
//
// Never open EventSource / stream `/api/events` from tab code — go
// through this facade (`subscribe` / `subscribeEvent` / `subscribeAll`).

import { createTransport, type SseTransport } from "../sse/transport";
import type { DaemonSnapshot, EventEnvelope } from "../sse/protocol";

const DAEMONS_KEY = "kb:daemons";

export type Phase = "idle" | "indexing" | "degraded" | "disconnected";

export type DaemonStatus = {
  url: string;
  phase: Phase;
  inFlight: number;
  openErrors: number;
  /// Sum of open comments across all artifacts the daemon has emitted
  /// `comments.updated` events for in this session.
  openComments: number;
  lastEventAt: number | null; // unix ms
  /// CT — the last reconnect attempt for this daemon looked auth-shaped
  /// (see sse/core.ts's `isAuthSuspect`), not a plain outage.
  authSuspect: boolean;
};

export type AggregatedStatus = {
  phase: Phase;
  inFlight: number;
  openErrors: number;
  openComments: number;
  /// True iff ANY daemon's last reconnect attempt was auth-suspect —
  /// the single-daemon common case reads this directly; a fleet view
  /// wanting per-daemon detail reads `daemons[i].authSuspect`.
  authSuspect: boolean;
  daemons: DaemonStatus[];
};

type Subscriber = (s: AggregatedStatus) => void;

/// Per-event subscription callback. Called once for each SSE frame of
/// the subscribed type, regardless of which daemon emitted it. The
/// payload is the inner `payload` field of the daemon's envelope; the
/// daemon URL is included so multi-daemon consumers can scope replies.
export type EventCallback = (
  payload: Record<string, unknown>,
  daemonUrl: string,
) => void;

/// Firehose subscription callback — every frame from every daemon,
/// including synthetic `lag`/`gap`, as a full envelope. Used by the
/// Live dashboard tab.
export type AllCallback = (e: EventEnvelope) => void;

class SseManager {
  private transport: SseTransport | null = null;
  /// Latest per-daemon snapshot from the core, keyed by URL. The facade
  /// aggregates these into AggregatedStatus; raw reducer state (runs
  /// Set, per-artifact comment Map) never leaves the core.
  private snapshots = new Map<string, DaemonSnapshot>();
  private subscribers = new Set<Subscriber>();
  /// Per-event-type subscribers. Dispatch is generic — any type the
  /// daemon emits reaches its listeners; no client-side type list to
  /// keep in sync, and types subscribed after connect just work.
  private eventListeners = new Map<string, Set<EventCallback>>();
  private allListeners = new Set<AllCallback>();
  /// Resync subscribers — fired when a daemon's stream reports a `gap`
  /// (resume cursor unusable: long disconnect or daemon restart). The
  /// events inside the gap are unknowable, so server-state stores
  /// refetch wholesale. Client-side notion only — no daemon event type
  /// named "resync" exists.
  private resyncListeners = new Set<() => void>();

  /// Returns the persisted daemon list, defaulting to a single same-origin
  /// daemon if none configured. The default uses window.location.host so
  /// the SPA works out-of-box when served from the daemon directly.
  loadDaemonUrls(): string[] {
    try {
      const raw = localStorage.getItem(DAEMONS_KEY);
      if (raw) {
        const parsed = JSON.parse(raw);
        if (Array.isArray(parsed) && parsed.every((s) => typeof s === "string")) {
          return parsed;
        }
      }
    } catch {
      // fallthrough to default
    }
    return [`${window.location.protocol}//${window.location.host}`];
  }

  saveDaemonUrls(urls: string[]) {
    localStorage.setItem(DAEMONS_KEY, JSON.stringify(urls));
    this.applyDaemons(urls);
  }

  start() {
    this.applyDaemons(this.loadDaemonUrls());
  }

  stop() {
    this.transport?.stop();
    this.transport = null;
    this.snapshots.clear();
  }

  /// Which host is running the connections — "shared" (SharedWorker),
  /// "direct" (in-tab fallback), or "none" before start(). Exposed for
  /// e2e assertions + debugging via window.__KB_SSE__.
  transportMode(): "shared" | "direct" | "none" {
    return this.transport?.mode ?? "none";
  }

  /// Current aggregate, without subscribing. Debug/e2e surface.
  currentStatus(): AggregatedStatus {
    return this.aggregate();
  }

  subscribe(fn: Subscriber): () => void {
    this.subscribers.add(fn);
    fn(this.aggregate());
    return () => {
      this.subscribers.delete(fn);
    };
  }

  /// Subscribe to a specific SSE event type. The callback fires for
  /// every frame of that type from any configured daemon. Returns an
  /// unsubscribe function.
  subscribeEvent(type: string, fn: EventCallback): () => void {
    let set = this.eventListeners.get(type);
    if (!set) {
      set = new Set();
      this.eventListeners.set(type, set);
    }
    set.add(fn);
    return () => {
      this.eventListeners.get(type)?.delete(fn);
    };
  }

  /// Subscribe to EVERY frame from every daemon (full envelopes,
  /// including synthetic lag/gap). Powers the Live dashboard tab.
  subscribeAll(fn: AllCallback): () => void {
    this.allListeners.add(fn);
    return () => {
      this.allListeners.delete(fn);
    };
  }

  /// Subscribe to gap-driven resyncs (see `resyncListeners`). Returns
  /// an unsubscribe function, mirroring `subscribeEvent`.
  onResync(fn: () => void): () => void {
    this.resyncListeners.add(fn);
    return () => {
      this.resyncListeners.delete(fn);
    };
  }

  private applyDaemons(urls: string[]) {
    if (!this.transport) {
      this.transport = createTransport({
        daemons: urls,
        sink: {
          event: (e) => this.onEvent(e),
          status: (s) => this.onStatus(s),
          snapshot: (list) => this.onSnapshot(list),
          resync: () => this.onResyncFrame(),
        },
      });
    } else {
      this.transport.setDaemons(urls);
    }
    // Prune snapshots for daemons that left the configured list — the
    // facade owns the list, the transport only reports on daemons it runs.
    const want = new Set(urls);
    for (const url of Array.from(this.snapshots.keys())) {
      if (!want.has(url)) this.snapshots.delete(url);
    }
    this.publish();
  }

  private onEvent(e: EventEnvelope) {
    for (const fn of this.eventListeners.get(e.type) ?? []) {
      fn(e.payload, e.daemonUrl);
    }
    for (const fn of this.allListeners) fn(e);
  }

  private onStatus(s: DaemonSnapshot) {
    this.snapshots.set(s.url, s);
    this.publish();
  }

  /// Full state replace — the worker's reply to a port join (`hello`) or
  /// a `set-daemons`. Gives a late-joining tab correct status (in-flight
  /// counts, active runs) accumulated before this tab existed.
  private onSnapshot(list: DaemonSnapshot[]) {
    this.snapshots = new Map(list.map((s) => [s.url, s]));
    this.publish();
  }

  private onResyncFrame() {
    console.warn("[sse] event-id gap — cursor cleared, stores resyncing");
    for (const fn of this.resyncListeners) fn();
  }

  private aggregate(): AggregatedStatus {
    const daemons: DaemonStatus[] = [];
    let inFlight = 0;
    let openErrors = 0;
    let openComments = 0;
    let authSuspect = false;
    let phase: Phase = "idle";
    for (const s of this.snapshots.values()) {
      let p: Phase;
      if (!s.connected) p = "disconnected";
      else if (s.openErrors > 0) p = "degraded";
      else if (s.activeRuns > 0) p = "indexing";
      else p = "idle";
      // `authSuspect` is optional on the wire snapshot (additive worker
      // protocol field, protocol.ts) — absent reads as false, never
      // undefined leaking into a boolean-typed field here.
      const suspect = s.authSuspect ?? false;
      daemons.push({
        url: s.url,
        phase: p,
        inFlight: s.inFlight,
        openErrors: s.openErrors,
        openComments: s.openComments,
        lastEventAt: s.lastEventAt,
        authSuspect: suspect,
      });
      inFlight += s.inFlight;
      openErrors += s.openErrors;
      openComments += s.openComments;
      if (suspect) authSuspect = true;
      if (severity(p) > severity(phase)) phase = p;
    }
    return { phase, inFlight, openErrors, openComments, authSuspect, daemons };
  }

  private publish() {
    const snapshot = this.aggregate();
    for (const sub of this.subscribers) sub(snapshot);
  }
}

function severity(p: Phase): number {
  switch (p) {
    case "idle":
      return 0;
    case "indexing":
      return 1;
    case "degraded":
      return 2;
    case "disconnected":
      return 3;
  }
}

export const sse = new SseManager();

declare global {
  interface Window {
    /// Debug + e2e surface: which transport carries SSE, and the
    /// current per-daemon status. Read-only.
    __KB_SSE__?: {
      transport: () => "shared" | "direct" | "none";
      status: () => AggregatedStatus;
    };
  }
}

if (typeof window !== "undefined") {
  window.__KB_SSE__ = {
    transport: () => sse.transportMode(),
    status: () => sse.currentStatus(),
  };
}
