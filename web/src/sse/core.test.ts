import { afterEach, describe, expect, it, vi } from "vitest";
import { SseCore, memoryCursorStore, type CoreSink } from "./core";
import type { DaemonSnapshot, EventEnvelope } from "./protocol";

const enc = new TextEncoder();
const URL_A = "http://127.0.0.1:4737";
const URL_B = "http://127.0.0.1:4738";

type Conn = {
  url: string;
  push: (s: string) => void;
  end: () => void;
  fail: () => void;
};

/// One controlled "the next fetch looks auth-suspect" response — a
/// status, a redirect landing url, and/or a content-type, whichever the
/// scenario needs. `respondSuspectNext` consumes exactly one attempt.
type SuspectSpec = {
  status?: number;
  redirectedUrl?: string;
  contentType?: string;
};

/// A controllable `/api/events` endpoint: every fetch records a Conn the
/// test can push SSE text into, end (server shutdown), or error (network
/// drop). `failNext` makes upcoming connection attempts return HTTP 500.
/// Every response carries `headers`/`redirected`/`url` (real `Response`
/// shape) — `core.ts`'s auth-suspect detection reads all three, and a
/// normal stream's `content-type: text/event-stream` must be present or
/// EVERY successful connection in this file would misread as suspect.
function fakeDaemon() {
  const conns: Conn[] = [];
  let failNext = 0;
  let suspectNext: SuspectSpec | null = null;
  const fetchImpl = (async (input: RequestInfo | URL, init?: RequestInit) => {
    if (suspectNext) {
      const s = suspectNext;
      suspectNext = null;
      const status = s.status ?? 200;
      return {
        ok: status >= 200 && status < 300,
        status,
        body: null,
        headers: new Headers(s.contentType ? { "content-type": s.contentType } : {}),
        redirected: !!s.redirectedUrl,
        url: s.redirectedUrl ?? String(input),
      } as unknown as Response;
    }
    if (failNext > 0) {
      failNext--;
      return {
        ok: false,
        status: 500,
        body: null,
        headers: new Headers(),
        redirected: false,
        url: String(input),
      } as unknown as Response;
    }
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
    const conn: Conn = {
      url: String(input),
      push: (s) => safely(() => controller.enqueue(enc.encode(s))),
      end: () => safely(() => controller.close()),
      fail: () => safely(() => controller.error(new Error("net"))),
    };
    conns.push(conn);
    return {
      ok: true,
      status: 200,
      body,
      headers: new Headers({ "content-type": "text/event-stream" }),
      redirected: false,
      url: String(input),
    } as unknown as Response;
  }) as typeof fetch;
  return {
    fetchImpl,
    conns,
    failNextConnects: (n: number) => {
      failNext = n;
    },
    respondSuspectNext: (s: SuspectSpec) => {
      suspectNext = s;
    },
  };
}

function recorder() {
  const events: EventEnvelope[] = [];
  const statuses: DaemonSnapshot[] = [];
  const resyncs: string[] = [];
  const sink: CoreSink = {
    event: (e) => events.push(e),
    status: (s) => statuses.push(s),
    resync: (u) => resyncs.push(u),
  };
  return { sink, events, statuses, resyncs };
}

/// Drain microtasks so pump() progresses past its awaits — works under
/// both real and fake timers (fetch resolution is microtask-only).
async function settle(rounds = 10) {
  for (let i = 0; i < rounds; i++) await Promise.resolve();
}

function frame(id: number, type: string, payload: unknown): string {
  const data = JSON.stringify({ v: "0.0.1", ts: "t", payload });
  return `id: ${id}\nevent: ${type}\ndata: ${data}\n\n`;
}

afterEach(() => {
  vi.useRealTimers();
});

describe("SseCore", () => {
  it("connects one unfiltered stream per daemon and reports status", async () => {
    const d = fakeDaemon();
    const r = recorder();
    const core = new SseCore({
      cursors: memoryCursorStore(),
      sink: r.sink,
      fetchImpl: d.fetchImpl,
    });
    core.setDaemons([URL_A, URL_B]);
    await settle();

    expect(d.conns.map((c) => c.url)).toEqual([
      `${URL_A}/api/events`,
      `${URL_B}/api/events`,
    ]);
    // Initial disconnected snapshot per daemon, then connected:true.
    const a = r.statuses.filter((s) => s.url === URL_A);
    expect(a[0].connected).toBe(false);
    expect(a[a.length - 1].connected).toBe(true);
    core.stop();
  });

  it("dispatches every frame as an envelope with the unwrapped payload", async () => {
    const d = fakeDaemon();
    const r = recorder();
    const core = new SseCore({
      cursors: memoryCursorStore(),
      sink: r.sink,
      fetchImpl: d.fetchImpl,
    });
    core.setDaemons([URL_A]);
    await settle();
    d.conns[0].push(frame(1, "note.created", { id: "n1" }));
    d.conns[0].push(frame(2, "metrics.tick", { requests_total: 9 }));
    await settle();

    expect(r.events.map((e) => e.type)).toEqual(["note.created", "metrics.tick"]);
    expect(r.events[0].payload).toEqual({ id: "n1" });
    expect(r.events[0].daemonUrl).toBe(URL_A);
    expect(r.events[1].id).toBe("2");
    core.stop();
  });

  it("advances the cursor per frame and resumes with last_event_id", async () => {
    vi.useFakeTimers();
    const d = fakeDaemon();
    const r = recorder();
    const cursors = memoryCursorStore();
    const core = new SseCore({
      cursors,
      sink: r.sink,
      fetchImpl: d.fetchImpl,
      random: () => 0.5, // pin jitter to 1.0x — this test asserts exact timing
    });
    core.setDaemons([URL_A]);
    await settle();
    d.conns[0].push(frame(41, "query", {}));
    d.conns[0].push(frame(42, "query", {}));
    await settle();
    expect(cursors.get(URL_A)).toBe("42");

    d.conns[0].fail();
    await settle();
    await vi.advanceTimersByTimeAsync(1000);
    expect(d.conns).toHaveLength(2);
    expect(d.conns[1].url).toBe(`${URL_A}/api/events?last_event_id=42`);
    core.stop();
  });

  // invariant:24
  it("clears the cursor and emits resync on a gap frame", async () => {
    const d = fakeDaemon();
    const r = recorder();
    const cursors = memoryCursorStore({ [URL_A]: "10" });
    const core = new SseCore({ cursors, sink: r.sink, fetchImpl: d.fetchImpl });
    core.setDaemons([URL_A]);
    await settle();
    d.conns[0].push(
      'event: gap\ndata: {"requested_id":10,"oldest_available_id":900}\n\n',
    );
    await settle();

    expect(cursors.get(URL_A)).toBeNull();
    expect(r.resyncs).toEqual([URL_A]);
    // The gap also flows to the firehose so the Live tab can render it.
    expect(r.events.map((e) => e.type)).toEqual(["gap"]);
    expect(r.events[0].payload).toEqual({
      requested_id: 10,
      oldest_available_id: 900,
    });
    core.stop();
  });

  it("emits lag frames to the firehose without resync", async () => {
    const d = fakeDaemon();
    const r = recorder();
    const core = new SseCore({
      cursors: memoryCursorStore(),
      sink: r.sink,
      fetchImpl: d.fetchImpl,
    });
    core.setDaemons([URL_A]);
    await settle();
    d.conns[0].push('event: lag\ndata: {"skipped":7}\n\n');
    await settle();

    expect(r.events.map((e) => e.type)).toEqual(["lag"]);
    expect(r.resyncs).toEqual([]);
    core.stop();
  });

  it("doubles backoff per failure up to the cap and resets on success", async () => {
    vi.useFakeTimers();
    const d = fakeDaemon();
    const r = recorder();
    const core = new SseCore({
      cursors: memoryCursorStore(),
      sink: r.sink,
      fetchImpl: d.fetchImpl,
      // Pin jitter's multiplier to exactly 1.0 (0.75 + 0.5*0.5) so this
      // test's exact-millisecond assertions are unaffected by it — the
      // jitter RANGE itself is asserted separately below.
      random: () => 0.5,
    });
    d.failNextConnects(3);
    core.setDaemons([URL_A]);
    await settle();
    expect(d.conns).toHaveLength(0); // first attempt got HTTP 500

    // Failure #1 → retry after 1s.
    await vi.advanceTimersByTimeAsync(999);
    await settle();
    await vi.advanceTimersByTimeAsync(1);
    await settle();
    // Failure #2 → retry after 2s.
    await vi.advanceTimersByTimeAsync(1999);
    await settle();
    await vi.advanceTimersByTimeAsync(1);
    await settle();
    // Failure #3 → retry after 4s; this attempt SUCCEEDS.
    await vi.advanceTimersByTimeAsync(4000);
    await settle();
    expect(d.conns).toHaveLength(1);

    // Success reset the backoff: next drop retries after 1s again.
    d.conns[0].fail();
    await settle();
    await vi.advanceTimersByTimeAsync(1000);
    expect(d.conns).toHaveLength(2);
    core.stop();
  });

  it("jitters the slept delay within ±25% without disturbing the backoff doubling sequence", async () => {
    vi.useFakeTimers();
    const d = fakeDaemon();
    const r = recorder();
    let nextRandom = 0;
    const core = new SseCore({
      cursors: memoryCursorStore(),
      sink: r.sink,
      fetchImpl: d.fetchImpl,
      random: () => nextRandom,
    });
    d.failNextConnects(2);
    core.setDaemons([URL_A]);
    // Attempt #1 fires + fails synchronously with random()=0 (floor):
    // backoff=1000 → slept delay = 1000*0.75 = 750ms, not the un-jittered
    // 1000ms.
    await settle();
    expect(d.conns).toHaveLength(0);

    // Ceiling for attempt #2's OWN retry, set before it fires.
    nextRandom = 1;
    await vi.advanceTimersByTimeAsync(749);
    await settle();
    expect(d.conns).toHaveLength(0); // attempt #2 hasn't fired yet
    await vi.advanceTimersByTimeAsync(1);
    await settle();
    expect(d.conns).toHaveLength(0); // attempt #2 fired and ALSO failed

    // The doubling sequence itself is untouched by jitter: attempt #2's
    // un-jittered backoff is exactly 2000 (doubled from 1000, not derived
    // from the jittered 750ms sleep). Ceiling: 2000*1.25 = 2500ms.
    await vi.advanceTimersByTimeAsync(2499);
    await settle();
    expect(d.conns).toHaveLength(0);
    await vi.advanceTimersByTimeAsync(1);
    await settle();
    expect(d.conns).toHaveLength(1); // attempt #3 succeeds (failNext spent)
    core.stop();
  });

  it("tracks in-flight balance and run lifecycle in the reducer", async () => {
    const d = fakeDaemon();
    const r = recorder();
    const core = new SseCore({
      cursors: memoryCursorStore(),
      sink: r.sink,
      fetchImpl: d.fetchImpl,
    });
    core.setDaemons([URL_A]);
    await settle();
    const c = d.conns[0];
    c.push(frame(1, "index.start", { run: "r1" }));
    c.push(frame(2, "index.file", {}));
    c.push(frame(3, "index.file", {}));
    c.push(frame(4, "artifact.indexed", {}));
    await settle();

    let last = r.statuses[r.statuses.length - 1];
    expect(last.activeRuns).toBe(1);
    expect(last.inFlight).toBe(1);

    c.push(frame(5, "index.complete", { run: "r1" }));
    await settle();
    last = r.statuses[r.statuses.length - 1];
    expect(last.activeRuns).toBe(0);
    expect(last.inFlight).toBe(0); // last run gone → in-flight zeroed
    core.stop();
  });

  it("counts open comments per artifact without double-counting re-emits", async () => {
    const d = fakeDaemon();
    const r = recorder();
    const core = new SseCore({
      cursors: memoryCursorStore(),
      sink: r.sink,
      fetchImpl: d.fetchImpl,
    });
    core.setDaemons([URL_A]);
    await settle();
    const c = d.conns[0];
    c.push(frame(1, "comments.updated", { artifact_id: "a1", open_count: 2 }));
    c.push(frame(2, "comments.updated", { artifact_id: "a2", open_count: 1 }));
    c.push(frame(3, "comments.updated", { artifact_id: "a1", open_count: 3 }));
    await settle();
    expect(r.statuses[r.statuses.length - 1].openComments).toBe(4);

    c.push(frame(4, "comments.updated", { artifact_id: "a1", open_count: 0 }));
    await settle();
    expect(r.statuses[r.statuses.length - 1].openComments).toBe(1);
    core.stop();
  });

  it("balances error counts with dismissed/fixed, floored at zero", async () => {
    const d = fakeDaemon();
    const r = recorder();
    const core = new SseCore({
      cursors: memoryCursorStore(),
      sink: r.sink,
      fetchImpl: d.fetchImpl,
    });
    core.setDaemons([URL_A]);
    await settle();
    const c = d.conns[0];
    c.push(frame(1, "error", {}));
    c.push(frame(2, "error", {}));
    c.push(frame(3, "error.dismissed", {}));
    c.push(frame(4, "error.fixed", {}));
    c.push(frame(5, "error.fixed", {}));
    await settle();
    expect(r.statuses[r.statuses.length - 1].openErrors).toBe(0);
    core.stop();
  });

  it("reconciles daemon removals by aborting their stream", async () => {
    const d = fakeDaemon();
    const r = recorder();
    const core = new SseCore({
      cursors: memoryCursorStore(),
      sink: r.sink,
      fetchImpl: d.fetchImpl,
    });
    core.setDaemons([URL_A, URL_B]);
    await settle();
    expect(core.snapshots().map((s) => s.url)).toEqual([URL_A, URL_B]);

    core.setDaemons([URL_A]);
    await settle();
    expect(core.snapshots().map((s) => s.url)).toEqual([URL_A]);

    // Frames from the aborted daemon no longer dispatch.
    d.conns[1].push(frame(1, "query", {}));
    await settle();
    expect(r.events).toEqual([]);
    core.stop();
  });

  it("treats a server-ended stream as a disconnect and reconnects", async () => {
    vi.useFakeTimers();
    const d = fakeDaemon();
    const r = recorder();
    const core = new SseCore({
      cursors: memoryCursorStore(),
      sink: r.sink,
      fetchImpl: d.fetchImpl,
      random: () => 0.5, // pin jitter to 1.0x — this test asserts exact timing
    });
    core.setDaemons([URL_A]);
    await settle();
    d.conns[0].end(); // daemon shutdown closes SSE via take_until
    await settle();

    const last = r.statuses[r.statuses.length - 1];
    expect(last.connected).toBe(false);
    await vi.advanceTimersByTimeAsync(1000);
    expect(d.conns).toHaveLength(2);
    core.stop();
  });

  it("ignores malformed envelope JSON without crashing the stream", async () => {
    const d = fakeDaemon();
    const r = recorder();
    const core = new SseCore({
      cursors: memoryCursorStore(),
      sink: r.sink,
      fetchImpl: d.fetchImpl,
    });
    core.setDaemons([URL_A]);
    await settle();
    d.conns[0].push("event: query\ndata: not-json\n\n");
    d.conns[0].push(frame(2, "query", { ok: true }));
    await settle();

    expect(r.events.map((e) => e.type)).toEqual(["query"]);
    expect(r.events[0].payload).toEqual({ ok: true });
    core.stop();
  });
});

describe("SseCore — auth-suspect detection (CT)", () => {
  it("flags a 401 reconnect response", async () => {
    const d = fakeDaemon();
    const r = recorder();
    const core = new SseCore({
      cursors: memoryCursorStore(),
      sink: r.sink,
      fetchImpl: d.fetchImpl,
    });
    d.respondSuspectNext({ status: 401 });
    core.setDaemons([URL_A]);
    await settle();

    const last = r.statuses[r.statuses.length - 1];
    expect(last.authSuspect).toBe(true);
    expect(last.connected).toBe(false); // still just a failed attempt
    core.stop();
  });

  it("flags a 403 reconnect response", async () => {
    const d = fakeDaemon();
    const r = recorder();
    const core = new SseCore({
      cursors: memoryCursorStore(),
      sink: r.sink,
      fetchImpl: d.fetchImpl,
    });
    d.respondSuspectNext({ status: 403 });
    core.setDaemons([URL_A]);
    await settle();

    expect(r.statuses[r.statuses.length - 1].authSuspect).toBe(true);
    core.stop();
  });

  it("flags a redirect that lands on a different origin (an SSO login page)", async () => {
    const d = fakeDaemon();
    const r = recorder();
    const core = new SseCore({
      cursors: memoryCursorStore(),
      sink: r.sink,
      fetchImpl: d.fetchImpl,
    });
    d.respondSuspectNext({
      status: 200,
      redirectedUrl: "https://auth.example.com/login",
      contentType: "text/html",
    });
    core.setDaemons([URL_A]);
    await settle();

    expect(r.statuses[r.statuses.length - 1].authSuspect).toBe(true);
    core.stop();
  });

  it("flags a 2xx same-origin response whose body isn't an event stream", async () => {
    const d = fakeDaemon();
    const r = recorder();
    const core = new SseCore({
      cursors: memoryCursorStore(),
      sink: r.sink,
      fetchImpl: d.fetchImpl,
    });
    d.respondSuspectNext({ status: 200, contentType: "text/html" });
    core.setDaemons([URL_A]);
    await settle();

    expect(r.statuses[r.statuses.length - 1].authSuspect).toBe(true);
    core.stop();
  });

  it("does NOT flag an ordinary 500 (a daemon crash is not a login page)", async () => {
    const d = fakeDaemon();
    const r = recorder();
    const core = new SseCore({
      cursors: memoryCursorStore(),
      sink: r.sink,
      fetchImpl: d.fetchImpl,
    });
    d.failNextConnects(1);
    core.setDaemons([URL_A]);
    await settle();

    expect(r.statuses.some((s) => s.authSuspect)).toBe(false);
    core.stop();
  });

  it("a normal successful connect never carries the flag (the fakeDaemon baseline)", async () => {
    const d = fakeDaemon();
    const r = recorder();
    const core = new SseCore({
      cursors: memoryCursorStore(),
      sink: r.sink,
      fetchImpl: d.fetchImpl,
    });
    core.setDaemons([URL_A]);
    await settle();

    const last = r.statuses[r.statuses.length - 1];
    expect(last.connected).toBe(true);
    expect(last.authSuspect).toBe(false);
    core.stop();
  });

  it("clears the flag once a subsequent reconnect actually succeeds", async () => {
    vi.useFakeTimers();
    const d = fakeDaemon();
    const r = recorder();
    const core = new SseCore({
      cursors: memoryCursorStore(),
      sink: r.sink,
      fetchImpl: d.fetchImpl,
      random: () => 0.5,
    });
    d.respondSuspectNext({ status: 401 });
    core.setDaemons([URL_A]);
    await settle();
    expect(r.statuses[r.statuses.length - 1].authSuspect).toBe(true);

    // The session got renewed — the NEXT attempt (after backoff) succeeds
    // normally.
    await vi.advanceTimersByTimeAsync(1000);
    await settle();

    const last = r.statuses[r.statuses.length - 1];
    expect(last.connected).toBe(true);
    expect(last.authSuspect).toBe(false);
    core.stop();
  });

  it("surfaces the flag on core.snapshots() too, not just the status sink", async () => {
    const d = fakeDaemon();
    const r = recorder();
    const core = new SseCore({
      cursors: memoryCursorStore(),
      sink: r.sink,
      fetchImpl: d.fetchImpl,
    });
    d.respondSuspectNext({ status: 401 });
    core.setDaemons([URL_A]);
    await settle();

    expect(core.snapshots()[0].authSuspect).toBe(true);
    core.stop();
  });
});
