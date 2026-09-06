// SseCore — per-daemon connection manager, context-agnostic (no DOM, no
// localStorage, no React). Hosted EITHER inline in a tab (direct mode)
// or inside the SharedWorker (SW2) — one code path, two hosts. The host
// supplies a CursorStore (localStorage-backed in tabs, in-memory in the
// worker) and consumes a CoreSink.
//
// One unfiltered `/api/events` stream per daemon. No `?types=` filter:
// the old MAIN_EVENT_TYPES list existed to spare idle tabs the 1Hz
// metrics tick, but it forced every new daemon event type to be listed
// client-side or live updates silently no-op'd. With a single shared
// stream the tick is one tiny frame per second — filtering is now the
// subscriber's business, not the transport's.
//
// Backoff: initial 1s, doubles per transport failure to 30s cap (±25%
// jitter, applied to the delay actually slept — the doubling sequence
// itself, and the cap, stay exact so backoff tests can assert on them),
// resets on a successful (re)connect. Reconnects resume via
// `?last_event_id=` from the CursorStore; a server `gap` frame (cursor
// fell out of the ring / daemon restarted) clears the cursor and emits
// resync — the events in between are unknowable, consumers refetch
// wholesale.
//
// CT — a reconnect response that LOOKS auth-shaped (401/403, a redirect
// to a different origin, or a 2xx that isn't actually an event stream —
// a forward-auth login page) sets `authSuspect` on the snapshot instead
// of the generic "reconnecting" story. Retries still run on the same
// backoff; this only changes what the UI says while they do.

import type { DaemonSnapshot, EventEnvelope } from "./protocol";
import { readSseStream, type SseFrame } from "./stream";

const BACKOFF_INITIAL_MS = 1000;
const BACKOFF_MAX_MS = 30_000;
/// ±25% jitter on the slept delay — thundering-herd insurance for a fleet
/// of tabs/daemons that all failed at the same instant (a daemon bounce,
/// a network blip). The random source (`SseCore`'s `random` option) is
/// injectable so tests can pin the spread instead of asserting a range.
const JITTER_SPREAD = 0.5; // delay * (0.75 .. 1.25)
const JITTER_FLOOR = 0.75;

/// Where resume cursors live. Tabs (direct mode) persist to localStorage
/// `kb:lid:<url>` so a reload resumes; the worker keeps them in memory
/// (it outlives reloads) with tabs mirroring for cold starts.
export type CursorStore = {
  get(url: string): string | null;
  set(url: string, id: string): void;
  clear(url: string): void;
};

export function memoryCursorStore(
  seed?: Record<string, string>,
): CursorStore {
  const map = new Map<string, string>(Object.entries(seed ?? {}));
  return {
    get: (url) => map.get(url) ?? null,
    set: (url, id) => void map.set(url, id),
    clear: (url) => void map.delete(url),
  };
}

export type CoreSink = {
  /// Every frame, unfiltered (including synthetic lag/gap).
  event(e: EventEnvelope): void;
  /// Per-daemon snapshot on connect/disconnect and on tracked events
  /// (the ones that change the reducer's scalars).
  status(s: DaemonSnapshot): void;
  /// Server gap — cursor already cleared; consumers refetch wholesale.
  resync(daemonUrl: string): void;
};

/// Event types whose presence affects the status reducer. Everything
/// else flows through `sink.event` untouched.
const TRACKED_TYPES = new Set([
  "index.start",
  "index.file",
  "index.embedding",
  "index.complete",
  "artifact.indexed",
  "error",
  "error.dismissed",
  "error.fixed",
  "comments.updated",
]);

type DaemonState = {
  url: string;
  abort: AbortController | null;
  reconnectTimer: ReturnType<typeof setTimeout> | null;
  backoff: number;
  connected: boolean;
  lastEventAt: number | null;
  runs: Set<string>;
  inFlight: number;
  openErrors: number;
  /// Latest open-count per artifact (comments.updated); the snapshot
  /// exposes the sum so a re-emit can't double-count.
  openCommentsByArtifact: Map<string, number>;
  /// CT — see the module-doc note above. Cleared on a genuine connect.
  authSuspect: boolean;
};

/// See the CT module-doc note above. Only the redirect/status checks are
/// unconditional (a real 401/403, or a redirect that lands on a
/// different origin, is auth-shaped no matter what); the content-type
/// check is scoped to `res.ok` so an ordinary 5xx (a daemon crash, not a
/// login page) never gets mislabeled — a ct-suspect flag only ever
/// changes the banner's COPY, never the retry behaviour, but a wrong
/// "reload to sign in" on a plain outage would be actively misleading.
function isAuthSuspect(res: Response, requestUrl: string): boolean {
  if (res.status === 401 || res.status === 403) return true;
  if (res.redirected && !sameOrigin(res.url, requestUrl)) return true;
  if (res.ok) {
    const ct = res.headers.get("content-type") ?? "";
    if (!ct.toLowerCase().includes("text/event-stream")) return true;
  }
  return false;
}

function sameOrigin(a: string, b: string): boolean {
  try {
    return new URL(a).origin === new URL(b, a).origin;
  } catch {
    return true; // can't tell — don't flag on a malformed URL
  }
}

export class SseCore {
  private daemons = new Map<string, DaemonState>();
  private cursors: CursorStore;
  private sink: CoreSink;
  private fetchImpl: typeof fetch;
  private stopped = false;
  /// Injectable so backoff-jitter tests can pin the spread instead of
  /// asserting on a range.
  private random: () => number;

  constructor(opts: {
    cursors: CursorStore;
    sink: CoreSink;
    fetchImpl?: typeof fetch;
    random?: () => number;
  }) {
    this.cursors = opts.cursors;
    this.sink = opts.sink;
    this.fetchImpl = opts.fetchImpl ?? fetch.bind(globalThis);
    this.random = opts.random ?? Math.random;
  }

  /// Bring connections in line with the URL list — open new ones, close
  /// removed ones, leave existing ones alone. Emits a status snapshot
  /// for each newly added daemon so consumers render rows immediately.
  setDaemons(urls: string[]) {
    const want = new Set(urls);
    for (const url of Array.from(this.daemons.keys())) {
      if (!want.has(url)) this.disconnectOne(url);
    }
    for (const url of urls) {
      if (!this.daemons.has(url)) this.connectOne(url);
    }
  }

  stop() {
    this.stopped = true;
    for (const url of Array.from(this.daemons.keys())) {
      this.disconnectOne(url);
    }
  }

  snapshots(): DaemonSnapshot[] {
    return Array.from(this.daemons.values(), (s) => this.snapshot(s));
  }

  private connectOne(url: string) {
    const state: DaemonState = {
      url,
      abort: null,
      reconnectTimer: null,
      backoff: BACKOFF_INITIAL_MS,
      connected: false,
      lastEventAt: null,
      runs: new Set(),
      inFlight: 0,
      openErrors: 0,
      openCommentsByArtifact: new Map(),
      authSuspect: false,
    };
    this.daemons.set(url, state);
    this.sink.status(this.snapshot(state));
    void this.pump(state);
  }

  private disconnectOne(url: string) {
    const state = this.daemons.get(url);
    if (!state) return;
    this.daemons.delete(url);
    state.abort?.abort();
    if (state.reconnectTimer) clearTimeout(state.reconnectTimer);
  }

  /// One connection attempt: fetch the stream, drain frames until the
  /// server ends it or the network drops, then schedule a retry. An
  /// abort from disconnectOne/stop ends the pump silently.
  private async pump(state: DaemonState) {
    const ac = new AbortController();
    state.abort = ac;
    const lid = this.cursors.get(state.url);
    const url =
      `${state.url}/api/events` +
      (lid ? `?last_event_id=${encodeURIComponent(lid)}` : "");
    try {
      // `same-origin`, NOT `omit`: a cookie-auth reverse proxy (prod's
      // traefik+Authelia forward-auth) 401s a cookie-less /api/events, which
      // silently killed EVERY live update on a deployed daemon — the
      // EventSource this replaced (SW0) always sent same-origin cookies.
      // Cross-origin fleet daemons stay cookie-less either way, so the
      // daemon's CORS layer never needs Access-Control-Allow-Credentials.
      const res = await this.fetchImpl(url, {
        signal: ac.signal,
        headers: { accept: "text/event-stream" },
        cache: "no-store",
        credentials: "same-origin",
      });
      const suspect = isAuthSuspect(res, url);
      if (suspect !== state.authSuspect) {
        // A failing daemon retries silently (no status delta to report);
        // an auth-shaped flip is surfaced immediately even though this
        // attempt is ABOUT to fail the same way every other failure does
        // — the UI needs to know within one attempt, not just on the
        // eventual connected→disconnected transition below (which never
        // fires at all for a daemon that was never connected yet).
        state.authSuspect = suspect;
        this.sink.status(this.snapshot(state));
      }
      if (!res.ok || !res.body || suspect) {
        throw new Error(`sse fetch ${res.status}`);
      }
      state.connected = true;
      state.backoff = BACKOFF_INITIAL_MS;
      this.sink.status(this.snapshot(state));
      await readSseStream(res.body, (frame) => this.handleFrame(state, frame));
      // Stream ended cleanly — daemon shutting down (take_until closes
      // SSE on SIGTERM). Fall through to the reconnect path.
    } catch {
      // Network error, non-2xx, or abort.
    }
    if (ac.signal.aborted || this.stopped || !this.daemons.has(state.url)) {
      return; // intentional teardown
    }
    if (state.connected) {
      state.connected = false;
      this.sink.status(this.snapshot(state));
    }
    // Jitter the SLEPT delay only — `state.backoff` itself keeps doubling
    // on the exact sequence (1s, 2s, 4s, … capped at 30s) so a caller
    // inspecting/asserting on it (and the next attempt's OWN jitter
    // range) stays predictable; only the wall-clock wait a fleet of
    // tabs/daemons actually sleeps gets spread, so they don't all retry
    // in lockstep.
    const delay = Math.round(
      state.backoff * (JITTER_FLOOR + this.random() * JITTER_SPREAD),
    );
    state.backoff = Math.min(state.backoff * 2, BACKOFF_MAX_MS);
    state.reconnectTimer = setTimeout(() => {
      state.reconnectTimer = null;
      void this.pump(state);
    }, delay);
  }

  private handleFrame(state: DaemonState, frame: SseFrame) {
    state.lastEventAt = Date.now();
    if (frame.event === "gap") {
      // Cursor fell out of the daemon's replay ring (long disconnect) or
      // is ahead of everything the daemon knows (daemon restarted). The
      // events in between are unknowable: drop the cursor so the next
      // (re)connect starts clean, and tell consumers to refetch.
      this.cursors.clear(state.url);
      this.sink.event(this.envelope(state, frame, parseLoose(frame.data)));
      this.sink.resync(state.url);
      return;
    }
    if (frame.event === "lag") {
      this.sink.event(this.envelope(state, frame, parseLoose(frame.data)));
      return;
    }
    // Regular daemon envelope: {v, ts, payload}. Single stream → every
    // frame advances the resume cursor (the old persistLid split between
    // main and metrics streams is moot).
    if (frame.id !== null) this.cursors.set(state.url, frame.id);
    let payload: Record<string, unknown> = {};
    try {
      const parsed = JSON.parse(frame.data);
      if (parsed && typeof parsed === "object" && "payload" in parsed) {
        payload = (parsed as { payload: Record<string, unknown> }).payload ?? {};
      }
    } catch {
      return; // malformed frame
    }
    this.sink.event(this.envelope(state, frame, payload));
    if (TRACKED_TYPES.has(frame.event)) {
      this.reduce(state, frame.event, payload);
      this.sink.status(this.snapshot(state));
    }
  }

  private envelope(
    state: DaemonState,
    frame: SseFrame,
    payload: Record<string, unknown>,
  ): EventEnvelope {
    return {
      daemonUrl: state.url,
      type: frame.event,
      payload,
      id: frame.id,
      at: state.lastEventAt ?? Date.now(),
    };
  }

  private reduce(
    state: DaemonState,
    kind: string,
    inner: Record<string, unknown>,
  ) {
    switch (kind) {
      case "index.start": {
        const run = (inner.run as string) ?? "";
        if (run) state.runs.add(run);
        break;
      }
      case "index.file": {
        // Count one in-flight item per `index.file`, balanced by
        // `artifact.indexed`. `index.embedding` is a sub-phase of an
        // already-counted `index.file`; counting it too over-reports.
        state.inFlight += 1;
        break;
      }
      case "artifact.indexed": {
        state.inFlight = Math.max(0, state.inFlight - 1);
        break;
      }
      case "index.complete": {
        const run = (inner.run as string) ?? "";
        if (run) state.runs.delete(run);
        if (state.runs.size === 0) state.inFlight = 0;
        break;
      }
      case "error": {
        state.openErrors += 1;
        break;
      }
      case "error.dismissed":
      case "error.fixed": {
        state.openErrors = Math.max(0, state.openErrors - 1);
        break;
      }
      case "comments.updated": {
        const aid = (inner.artifact_id as string) ?? "";
        const open = (inner.open_count as number) ?? 0;
        if (aid) {
          if (open > 0) {
            state.openCommentsByArtifact.set(aid, open);
          } else {
            state.openCommentsByArtifact.delete(aid);
          }
        }
        break;
      }
    }
  }

  private snapshot(state: DaemonState): DaemonSnapshot {
    let openComments = 0;
    for (const n of state.openCommentsByArtifact.values()) openComments += n;
    return {
      url: state.url,
      connected: state.connected,
      lastEventAt: state.lastEventAt,
      inFlight: state.inFlight,
      openErrors: state.openErrors,
      openComments,
      activeRuns: state.runs.size,
      authSuspect: state.authSuspect,
    };
  }
}

/// Synthetic frames (lag/gap) carry a bare JSON body, not the daemon
/// envelope. Tolerate anything.
function parseLoose(raw: string): Record<string, unknown> {
  try {
    const parsed = JSON.parse(raw);
    if (parsed && typeof parsed === "object") {
      return parsed as Record<string, unknown>;
    }
  } catch {
    // fallthrough
  }
  return { raw };
}
