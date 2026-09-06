// @vitest-environment jsdom
//
// CT — direct-mode (in-tab) bfcache parity: startShared already wires
// pagehide/pageshow (transport.ts's own module doc); this asserts startDirect
// gets the same restore-on-resume behaviour, adapted (no worker port to
// rejoin — the CORE itself froze with the page, so a persisted pageshow
// forces a disconnect+reconnect + resync instead of a port handshake).
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { createTransport, type TransportSink } from "./transport";
import type { DaemonSnapshot, EventEnvelope } from "./protocol";

type Conn = { url: string; fail: () => void };

/// Same shape as core.test.ts's fakeDaemon — a real `Response`-like object
/// (headers/redirected/url) whose body only ends when the request is
/// aborted, so `t.stop()` / a forced reconnect actually unblocks the
/// pending `readSseStream` read.
function fakeFetch() {
  const conns: Conn[] = [];
  const fetchImpl = (async (input: RequestInfo | URL, init?: RequestInit) => {
    let controller!: ReadableStreamDefaultController<Uint8Array>;
    const body = new ReadableStream<Uint8Array>({
      start(c) {
        controller = c;
      },
    });
    const safely = (f: () => void) => {
      try {
        f();
      } catch {
        // already closed/errored
      }
    };
    init?.signal?.addEventListener("abort", () =>
      safely(() => controller.error(new DOMException("x", "AbortError"))),
    );
    conns.push({
      url: String(input),
      fail: () => safely(() => controller.error(new Error("net"))),
    });
    return {
      ok: true,
      status: 200,
      body,
      headers: new Headers({ "content-type": "text/event-stream" }),
      redirected: false,
      url: String(input),
    } as unknown as Response;
  }) as typeof fetch;
  return { fetchImpl, conns };
}

function recorderSink() {
  const events: EventEnvelope[] = [];
  const statuses: DaemonSnapshot[] = [];
  let resyncCount = 0;
  const sink: TransportSink = {
    event: (e) => events.push(e),
    status: (s) => statuses.push(s),
    snapshot: () => {},
    resync: () => {
      resyncCount++;
    },
  };
  return { sink, events, statuses, resyncCount: () => resyncCount };
}

async function settle(rounds = 10) {
  for (let i = 0; i < rounds; i++) await Promise.resolve();
}

function pageshow(persisted: boolean) {
  window.dispatchEvent(new PageTransitionEvent("pageshow", { persisted }));
}

// vitest's jsdom environment doesn't expose a working `localStorage` global
// (Node 20.11+'s own built-in shadows jsdom's `window.localStorage` and
// needs a `--localstorage-file` flag this repo doesn't pass) — every other
// jsdom test here that touches storage stubs it explicitly; same pattern.
function stubLocalStorage() {
  const store = new Map<string, string>();
  vi.stubGlobal("localStorage", {
    getItem: (k: string) => (store.has(k) ? store.get(k)! : null),
    setItem: (k: string, v: string) => {
      store.set(k, v);
    },
    removeItem: (k: string) => {
      store.delete(k);
    },
    clear: () => store.clear(),
  });
}

beforeEach(() => {
  stubLocalStorage();
  // Force direct mode — the kill-switch documented at the top of
  // transport.ts — so this suite never touches a real SharedWorker.
  localStorage.setItem("kb:sse:transport", "direct");
});

afterEach(() => {
  vi.unstubAllGlobals();
});

describe("direct transport — bfcache restore parity", () => {
  it("forces a disconnect+reconnect and resyncs on a persisted pageshow", async () => {
    const f = fakeFetch();
    vi.stubGlobal("fetch", f.fetchImpl);
    const { sink, resyncCount } = recorderSink();
    const t = createTransport({ daemons: ["http://d1"], sink });
    expect(t.mode).toBe("direct");
    await settle();
    expect(f.conns).toHaveLength(1);
    expect(resyncCount()).toBe(0);

    pageshow(true);
    await settle();

    expect(f.conns).toHaveLength(2); // the stale connection dropped, a fresh one opened
    expect(resyncCount()).toBe(1);
    t.stop();
  });

  it("a non-persisted pageshow (an ordinary nav, not a bfcache restore) does nothing", async () => {
    const f = fakeFetch();
    vi.stubGlobal("fetch", f.fetchImpl);
    const { sink, resyncCount } = recorderSink();
    const t = createTransport({ daemons: ["http://d1"], sink });
    await settle();
    expect(f.conns).toHaveLength(1);

    pageshow(false);
    await settle();

    expect(f.conns).toHaveLength(1); // no reconnect
    expect(resyncCount()).toBe(0);
    t.stop();
  });

  it("reconnects EVERY configured daemon, not just the first", async () => {
    const f = fakeFetch();
    vi.stubGlobal("fetch", f.fetchImpl);
    const { sink } = recorderSink();
    const t = createTransport({ daemons: ["http://d1", "http://d2"], sink });
    await settle();
    expect(f.conns).toHaveLength(2);

    pageshow(true);
    await settle();

    expect(f.conns).toHaveLength(4);
    expect(new Set(f.conns.map((c) => c.url.split("?")[0].replace("/api/events", "")))).toEqual(
      new Set(["http://d1", "http://d2"]),
    );
    t.stop();
  });

  it("stop() removes the listeners — a later pageshow is a no-op", async () => {
    const f = fakeFetch();
    vi.stubGlobal("fetch", f.fetchImpl);
    const { sink, resyncCount } = recorderSink();
    const t = createTransport({ daemons: ["http://d1"], sink });
    await settle();
    t.stop();

    pageshow(true);
    await settle();

    expect(resyncCount()).toBe(0);
  });

  it("does not register duplicate listeners for a single transport instance", async () => {
    const f = fakeFetch();
    vi.stubGlobal("fetch", f.fetchImpl);
    const { sink, resyncCount } = recorderSink();
    const t = createTransport({ daemons: ["http://d1"], sink });
    await settle();

    pageshow(true);
    await settle();
    // Exactly one resync per persisted pageshow — a duplicate listener
    // would double-fire this.
    expect(resyncCount()).toBe(1);
    t.stop();
  });
});
