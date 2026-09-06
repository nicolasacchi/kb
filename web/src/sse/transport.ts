// Tab-side SSE transport selection. Two hosts for the same SseCore:
//
//  "shared" — a SharedWorker (workers/sse.worker.ts) owns ONE unfiltered
//             /api/events stream per daemon for ALL same-origin tabs; this
//             tab is a thin MessagePort client. Per-browser connection
//             count stops scaling with tab count — the cure for Firefox's
//             6-connections-per-host:port HTTP/1.1 pool, which parked SSE
//             sockets exhausted (new tabs hung forever).
//  "direct" — the identical core runs inline in this tab (one stream per
//             daemon per tab). The fallback is first-class: it is the
//             compat story (browsers without SharedWorker), the failure
//             containment (handshake timeout → degrade, never break), and
//             the debugging/test escape hatch (Playwright can't intercept
//             SharedWorker-initiated requests).
//
// Kill-switch to force direct mode: `?sse=direct` in the URL, or
// `localStorage["kb:sse:transport"] = "direct"`. Useful while iterating
// on sse/* in dev — HMR never reaches a running SharedWorker.
//
// Cursors: localStorage `kb:lid:<url>` is the durable home. In direct
// mode the core writes it via CursorStore. In shared mode the WORKER is
// the sole advancer (in-memory) — this tab only mirrors envelope ids
// back to localStorage (throttled) so a browser cold start can seed the
// next worker via `hello.cursors` instead of replaying the whole ring.

import { SseCore, type CursorStore } from "./core";
import {
  SSE_PROTOCOL_V,
  type DaemonSnapshot,
  type EventEnvelope,
  type TabToWorker,
  type WorkerToTab,
} from "./protocol";

const LID_PREFIX = "kb:lid:";
const TRANSPORT_KEY = "kb:sse:transport";
/// No `snapshot` reply within this window after `hello` → the worker is
/// dead, stale-generation, or unsupported; degrade to direct mode.
const HANDSHAKE_TIMEOUT_MS = 3000;
/// Throttle for mirroring the worker's cursor into localStorage.
const CURSOR_MIRROR_MS = 1000;

export type TransportSink = {
  event(e: EventEnvelope): void;
  status(s: DaemonSnapshot): void;
  /// Full per-daemon state replace — worker join / set-daemons reply.
  snapshot(daemons: DaemonSnapshot[]): void;
  resync(): void;
};

export type SseTransport = {
  readonly mode: "shared" | "direct";
  setDaemons(urls: string[]): void;
  stop(): void;
};

/// Direct-mode cursor store (and the shared-mode mirror target).
export const localCursors: CursorStore = {
  get(url) {
    try {
      return localStorage.getItem(LID_PREFIX + url);
    } catch {
      return null;
    }
  },
  set(url, id) {
    try {
      localStorage.setItem(LID_PREFIX + url, id);
    } catch {
      // private mode / quota — resume is best-effort
    }
  },
  clear(url) {
    try {
      localStorage.removeItem(LID_PREFIX + url);
    } catch {
      // ignore
    }
  },
};

function forcedDirect(): boolean {
  try {
    if (new URLSearchParams(window.location.search).get("sse") === "direct") {
      return true;
    }
    if (localStorage.getItem(TRANSPORT_KEY) === "direct") return true;
  } catch {
    // no window/localStorage — direct is the safe host
  }
  return false;
}

export function createTransport(opts: {
  daemons: string[];
  sink: TransportSink;
}): SseTransport {
  let active: SseTransport;
  const fallback = () => {
    console.warn("[sse] shared transport unavailable — running direct in-tab");
    active = startDirect(opts.daemons, opts.sink);
  };
  if (forcedDirect() || typeof SharedWorker === "undefined") {
    active = startDirect(opts.daemons, opts.sink);
  } else {
    try {
      active = startShared(opts.daemons, opts.sink, fallback);
    } catch {
      fallback();
    }
  }
  return {
    get mode() {
      return active.mode;
    },
    setDaemons: (urls) => active.setDaemons(urls),
    stop: () => active.stop(),
  };
}

function startDirect(daemons: string[], sink: TransportSink): SseTransport {
  let current = [...daemons];
  const core = new SseCore({
    cursors: localCursors,
    sink: {
      event: (e) => sink.event(e),
      status: (s) => sink.status(s),
      resync: () => sink.resync(),
    },
  });
  core.setDaemons(current);

  // CT — bfcache parity with startShared's pagehide/pageshow wiring above.
  // Direct mode has no separate worker to rejoin on restore; the CORE
  // itself is frozen WITH the page (its fetch reads and backoff timers
  // stop dead), and — unlike the worker, which keeps other tabs' streams
  // alive through one tab's freeze — nothing survives to notice the
  // socket died. Assume it's dead on restore: force every daemon through
  // a disconnect+reconnect (which also resets backoff, so the first retry
  // after a long freeze is immediate rather than resuming mid-backoff)
  // and treat it exactly like a worker-side gap — events during the
  // freeze are unknowable, so consumers refetch.
  const onPageshow = (ev: PageTransitionEvent) => {
    if (!ev.persisted) return;
    core.setDaemons([]);
    core.setDaemons(current);
    sink.resync();
  };
  // No action needed on the way down beyond what already happens (the
  // whole core freezes WITH the page — there's no separate worker to
  // detach from, and a genuine unload just drops the tab's connections
  // entirely). The listener exists anyway so direct mode has the SAME
  // pair startShared does, for one symmetric `stop()` cleanup path.
  const onPagehide = () => {};
  window.addEventListener("pageshow", onPageshow);
  window.addEventListener("pagehide", onPagehide);

  return {
    mode: "direct",
    setDaemons: (urls) => {
      current = [...urls];
      core.setDaemons(current);
    },
    stop: () => {
      window.removeEventListener("pageshow", onPageshow);
      window.removeEventListener("pagehide", onPagehide);
      core.stop();
    },
  };
}

function startShared(
  initialDaemons: string[],
  sink: TransportSink,
  fallback: () => void,
): SseTransport {
  let daemons = [...initialDaemons];
  let port: MessagePort | null = null;
  let handshake: ReturnType<typeof setTimeout> | null = null;
  let stopped = false;
  const mirroredAt = new Map<string, number>();

  const hello = (): TabToWorker => {
    const cursors: Record<string, string> = {};
    for (const url of daemons) {
      const lid = localCursors.get(url);
      if (lid) cursors[url] = lid;
    }
    return {
      kind: "hello",
      v: SSE_PROTOCOL_V,
      buildSha: typeof __KB_BUILD_SHA__ === "string" ? __KB_BUILD_SHA__ : "unknown",
      daemons,
      cursors,
    };
  };

  // Mirror the worker's cursor (rides every envelope) into localStorage,
  // throttled per daemon. Only the worker ADVANCES the cursor — every tab
  // mirrors the same monotonic value, which erases the old multi-tab race
  // where one tab's write skipped events another tab hadn't seen.
  const mirrorCursor = (e: EventEnvelope) => {
    if (e.id === null || e.type === "lag" || e.type === "gap") return;
    const now = Date.now();
    if (now - (mirroredAt.get(e.daemonUrl) ?? 0) < CURSOR_MIRROR_MS) return;
    mirroredAt.set(e.daemonUrl, now);
    localCursors.set(e.daemonUrl, e.id);
  };

  const handle = (msg: WorkerToTab) => {
    switch (msg.kind) {
      case "snapshot":
        if (handshake) {
          clearTimeout(handshake);
          handshake = null;
        }
        sink.snapshot(msg.daemons);
        break;
      case "status":
        sink.status(msg.daemon);
        break;
      case "event":
        mirrorCursor(msg.event);
        sink.event(msg.event);
        break;
      case "resync":
        // The worker cleared its own cursor; drop the stale mirror too.
        localCursors.clear(msg.daemonUrl);
        sink.resync();
        break;
    }
  };

  const connect = () => {
    // A SharedWorker object exposes exactly one port — (re)joining means
    // constructing a fresh one. Same (hashed) URL → same worker instance.
    const worker = new SharedWorker(
      new URL("../workers/sse.worker.ts", import.meta.url),
      { type: "module", name: "kb-sse" },
    );
    port = worker.port;
    port.onmessage = (ev: MessageEvent) => handle(ev.data as WorkerToTab);
    port.postMessage(hello());
    handshake = setTimeout(() => {
      // Dead or stale-generation worker (it ignores mismatched hellos).
      handshake = null;
      port?.close();
      port = null;
      if (!stopped) fallback();
    }, HANDSHAKE_TIMEOUT_MS);
  };

  const onPagehide = () => {
    // Belt-and-braces port cleanup beside the MessagePort `close` event
    // (whose cross-browser support is still uneven).
    try {
      port?.postMessage({ kind: "bye" } satisfies TabToWorker);
    } catch {
      // port already gone
    }
  };
  const onPageshow = (ev: PageTransitionEvent) => {
    if (!ev.persisted || stopped) return;
    // bfcache restore: this tab held a populated query cache
    // (staleTime: Infinity → no refetch) and missed every port message
    // while frozen. Rejoin with a fresh port and treat the restore
    // exactly like a gap — invalidate the world.
    port?.close();
    connect();
    sink.resync();
  };
  window.addEventListener("pagehide", onPagehide);
  window.addEventListener("pageshow", onPageshow);

  connect();

  return {
    mode: "shared",
    setDaemons: (urls) => {
      daemons = [...urls];
      try {
        port?.postMessage({ kind: "set-daemons", daemons } satisfies TabToWorker);
      } catch {
        // port gone; the next pageshow/connect re-sends via hello
      }
    },
    stop: () => {
      stopped = true;
      if (handshake) clearTimeout(handshake);
      window.removeEventListener("pagehide", onPagehide);
      window.removeEventListener("pageshow", onPageshow);
      onPagehide();
      port?.close();
      port = null;
    },
  };
}
