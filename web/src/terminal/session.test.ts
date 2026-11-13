// T14 — the pure stream machine: 12 pinned behaviours with fake
// effects (socket, tickets, timers, randomness). No xterm, no DOM.

import { describe, expect, it } from "vitest";
import { encodeBinaryFrame } from "./wire";
import {
  TermSession,
  nextDelay,
  type SessionEffects,
  type SessionSocket,
  type SocketHandlers,
  type TermSink,
} from "./session";
import { TermsApiError } from "../api/terms";

async function flush(): Promise<void> {
  for (let i = 0; i < 10; i += 1) await Promise.resolve();
}

interface FakeTimer {
  id: number;
  fn: () => void;
  ms: number;
  cleared: boolean;
  fired: boolean;
}

class FakeSocket implements SessionSocket {
  handlers: SocketHandlers;
  sent: string[] = [];
  closeCodes: number[] = [];
  constructor(handlers: SocketHandlers) {
    this.handlers = handlers;
  }
  send(data: string): void {
    this.sent.push(data);
  }
  close(code = 1000): void {
    this.closeCodes.push(code);
  }
  pushBinary(offset: number, bytes: Uint8Array | string): void {
    const payload = typeof bytes === "string" ? new TextEncoder().encode(bytes) : bytes;
    this.handlers.onMessageBinary(encodeBinaryFrame(offset, payload));
  }
  pushText(frame: unknown): void {
    this.handlers.onMessageText(typeof frame === "string" ? frame : JSON.stringify(frame));
  }
  closeFromServer(code: number): void {
    this.handlers.onClose(code);
  }
}

const PROFILE = {
  t: "profile", xterm: "6.0.0", serialize: "0.14.0", width: "unicode-graphemes@0.4.0",
  image: "0.9.0", cols: 120, rows: 40, scrollback: 50000,
  scroll_on_erase_in_display: true, cell_px_w: 9, cell_px_h: 18, theme: null,
};

function setup(opts: { random?: () => number } = {}) {
  const calls: Array<[string, ...unknown[]]> = [];
  const sink: TermSink = {
    write: (data, done) => {
      calls.push(["write", Array.from(data)]);
      done();
    },
    reset: () => calls.push(["reset"]),
    resize: (cols, rows) => calls.push(["resize", cols, rows]),
  };
  const timers: FakeTimer[] = [];
  let nextTimer = 1;
  const sockets: FakeSocket[] = [];
  const urls: string[] = [];
  let ticketSeq = 0;
  const mintedTickets: string[] = [];
  let mintError: unknown = null;
  const transients: string[] = [];
  const effects: SessionEffects = {
    openSocket: (url, handlers) => {
      urls.push(url);
      const s = new FakeSocket(handlers);
      sockets.push(s);
      return s;
    },
    mintTicket: async () => {
      if (mintError) throw mintError;
      ticketSeq += 1;
      mintedTickets.push(`tick-${ticketSeq}`);
      return { ticket: `tick-${ticketSeq}` };
    },
    now: () => 1_000_000,
    setTimer: (fn, ms) => {
      const t: FakeTimer = { id: nextTimer, fn, ms, cleared: false, fired: false };
      nextTimer += 1;
      timers.push(t);
      return t;
    },
    clearTimer: (h) => {
      (h as FakeTimer).cleared = true;
    },
    random: opts.random ?? (() => 0.5),
  };
  const session = new TermSession({
    termId: "t1",
    sink,
    effects,
    callbacks: {
      onState: () => {},
      onTransient: (m) => transients.push(m),
    },
  });
  const pendingTimer = (ms: number): FakeTimer | undefined =>
    timers.find((t) => t.ms === ms && !t.cleared && !t.fired);
  const fire = (ms: number): void => {
    const t = pendingTimer(ms);
    if (!t) throw new Error(`no pending timer for ${ms}ms (have ${timers.map((x) => x.ms).join(",")})`);
    t.fired = true;
    t.fn();
  };
  return {
    session, sink, calls, timers, sockets, urls, mintedTickets, transients,
    setMintError: (e: unknown) => {
      mintError = e;
    },
    pendingTimer, fire,
  };
}

/** Drive a fresh session to `live` via a full seed join. */
async function goLive(h: ReturnType<typeof setup>): Promise<FakeSocket> {
  h.session.start();
  await flush();
  const s = h.sockets[h.sockets.length - 1];
  s.pushText({ t: "hello", proto: "kb-term/1", session: "s1", epoch: "E1", harness: "omp", bound_sid: null });
  s.pushText(PROFILE);
  s.pushText({ t: "seed", offset: 100, len: 5 });
  s.pushBinary(100, "hello");
  await flush();
  return s;
}

describe("TermSession", () => {
  it("in-order data advances applied", async () => {
    const h = setup();
    const s = await goLive(h);
    s.pushBinary(105, "x");
    await flush();
    expect(h.session.getState().applied).toBe(106);
    expect(h.calls.at(-1)).toEqual(["write", [120]]);
  });

  it("absorbs a 37-byte rewind overlap exactly", async () => {
    const h = setup();
    const s = await goLive(h);
    // applied = 105; rewind 37 -> off 68, 50-byte payload.
    const payload = new TextEncoder().encode("A".repeat(50));
    s.pushBinary(68, payload);
    await flush();
    expect(h.session.getState().applied).toBe(105 + 13);
    expect(h.calls.at(-1)).toEqual(["write", Array.from(payload.subarray(37))]);
    expect(h.session.getState().gaps).toBe(0);
  });

  it("off > applied closes, discards applied and rejoins FULL (no from)", async () => {
    const h = setup();
    const s = await goLive(h);
    s.pushBinary(999, "far");
    expect(h.session.getState().gaps).toBe(1);
    expect(s.closeCodes).toEqual([1000]);
    h.fire(h.pendingTimer(250)?.ms ?? 250);
    await flush();
    const url = h.urls[h.urls.length - 1];
    expect(url).not.toContain("from=");
    expect(h.mintedTickets.length).toBe(2);
  });

  it("seed resets once; a resume (no seed) never resets", async () => {
    const h = setup();
    const s = await goLive(h);
    expect(h.calls.filter((c) => c[0] === "reset")).toHaveLength(1);
    expect(h.session.getState().resets).toBe(1);
    s.closeFromServer(1000);
    h.fire(h.pendingTimer(250)?.ms ?? 250);
    await flush();
    const s2 = h.sockets[h.sockets.length - 1];
    expect(h.urls[h.urls.length - 1]).toContain("from=105");
    s2.pushText({ t: "hello", proto: "kb-term/1", session: "s1", epoch: "E1", harness: "omp", bound_sid: null });
    s2.pushBinary(105, "x");
    expect(h.session.getState().resets).toBe(1);
    expect(h.calls.filter((c) => c[0] === "reset")).toHaveLength(1);
  });

  it("epoch change discards applied; the next connect omits from", async () => {
    const h = setup();
    const s = await goLive(h);
    s.closeFromServer(1000);
    h.fire(h.pendingTimer(250)?.ms ?? 250);
    await flush();
    expect(h.urls[1]).toContain("from=105");
    const s2 = h.sockets[1];
    s2.pushText({ t: "hello", proto: "kb-term/1", session: "s1", epoch: "E2", harness: "omp", bound_sid: null });
    expect(h.session.getState().applied).toBe(0);
    s2.closeFromServer(1000);
    h.fire(h.pendingTimer(500)?.ms ?? 500);
    await flush();
    expect(h.urls[2]).not.toContain("from=");
  });

  it("backoff is 250,500,1000,... capped at 10000 (jitter 0) and resets only after >=5s live", async () => {
    const h = setup({ random: () => 0.5 });
    const s = await goLive(h);
    s.closeFromServer(1000);
    // attempts 0..7: 250,500,1000,2000,4000,8000,10000,10000.
    const expected = [250, 500, 1000, 2000, 4000, 8000, 10000, 10000];
    for (let i = 0; i < expected.length; i += 1) {
      const ms = expected[i];
      expect(h.pendingTimer(ms), `step ${i} expects ${ms}ms`).toBeDefined();
      h.fire(ms);
      await flush();
      const sock = h.sockets[h.sockets.length - 1];
      sock.closeFromServer(1000);
    }
    expect(h.session.getState().attempt).toBe(8);
    // Live again, but attempt must NOT reset before 5s live elapses.
    const s2 = h.sockets[h.sockets.length - 1];
    s2.pushText({ t: "hello", proto: "kb-term/1", session: "s1", epoch: "E1", harness: "omp", bound_sid: null });
    // A resume join needs no seed; the first in-order data frame marks live.
    s2.pushBinary(105, "q");
    expect(h.session.getState().phase).toBe("live");
    expect(h.session.getState().attempt).toBe(8);
    h.fire(5000);
    expect(h.session.getState().attempt).toBe(0);
  });

  it("jitter scales the base delay to [0.8, 1.2)", async () => {
    const lo = setup({ random: () => 0 });
    const sLo = await goLive(lo);
    sLo.closeFromServer(1000);
    // round(250 * 0.8) = 200.
    expect(lo.pendingTimer(200)).toBeDefined();
    const hi = setup({ random: () => 0.999999 });
    const sHi = await goLive(hi);
    sHi.closeFromServer(1000);
    // round(250 * 1.2) = 300, never above the cap band.
    const delays = hi.timers.filter((t) => !t.cleared && !t.fired && t.ms >= 200 && t.ms <= 300).map((t) => t.ms);
    expect(delays).toContain(300);
  });

  it("a second data frame does not re-arm the 5s reset (armed once per generation)", async () => {
    const h = setup();
    const s = await goLive(h);
    s.closeFromServer(1000);
    h.fire(250);
    await flush();
    const s2 = h.sockets[h.sockets.length - 1];
    s2.pushText({ t: "hello", proto: "kb-term/1", session: "s1", epoch: "E1", harness: "omp", bound_sid: null });
    s2.pushBinary(105, "a");
    const armed = h.pendingTimer(5000);
    expect(armed).toBeDefined();
    // More frames on the same generation must keep the ORIGINAL timer.
    s2.pushBinary(106, "b");
    s2.pushBinary(107, "c");
    expect(h.pendingTimer(5000)).toBe(armed);
    expect(armed!.cleared).toBe(false);
    h.fire(5000);
    expect(h.session.getState().attempt).toBe(0);
  });

  it("a resumed (seedless) connection becomes live on its first data frame and accepts input", async () => {
    const h = setup();
    const s = await goLive(h);
    s.closeFromServer(1000);
    h.fire(h.pendingTimer(250)?.ms ?? 250);
    await flush();
    const s2 = h.sockets[h.sockets.length - 1];
    expect(h.urls[h.urls.length - 1]).toContain("from=105");
    s2.pushText({ t: "hello", proto: "kb-term/1", session: "s1", epoch: "E1", harness: "omp", bound_sid: null });
    // Not resumed until data arrives.
    expect(h.session.getState().phase).toBe("reconnecting");
    s2.pushBinary(105, "x");
    expect(h.session.getState().phase).toBe("live");
    expect(h.session.sendInput("y")).toBe(true);
    expect(s2.sent).toEqual(['{"t":"input","data":"y"}']);
  });

  it("mints exactly one new ticket per attempt", async () => {
    const h = setup();
    const s = await goLive(h);
    expect(h.mintedTickets).toEqual(["tick-1"]);
    s.closeFromServer(1000);
    h.fire(250);
    await flush();
    expect(h.mintedTickets).toEqual(["tick-1", "tick-2"]);
    expect(h.sockets).toHaveLength(2);
    expect(h.urls[1]).not.toContain("ticket=tick-1");
  });

  it("exit ends the stream permanently: no reconnect, no further tickets", async () => {
    const h = setup();
    const s = await goLive(h);
    s.pushText({ t: "exit", seq: 3, code: 3, offset: 105 });
    expect(h.session.getState().phase).toBe("ended");
    expect(h.session.getState().caption).toBe("exited (code 3)");
    expect(h.timers.filter((t) => !t.cleared && !t.fired)).toEqual([]);
    s.closeFromServer(1000);
    await flush();
    expect(h.mintedTickets).toEqual(["tick-1"]);
  });

  it("ticket 404 blocks with 'terminal disabled' and zero further attempts", async () => {
    const h = setup();
    h.setMintError(new TermsApiError(404, null, "404 Not Found"));
    h.session.start();
    await flush();
    expect(h.session.getState().phase).toBe("blocked");
    expect(h.session.getState().caption).toBe("blocked: terminal disabled");
    expect(h.timers.filter((t) => !t.cleared && !t.fired)).toEqual([]);
    expect(h.sockets).toHaveLength(0);
  });

  it("ticket 404 with a terminal-not-found problem blocks as 'terminal not found'", async () => {
    const h = setup();
    h.setMintError(new TermsApiError(404, "urn:kb:errors:terminal-not-found", "Not Found"));
    h.session.start();
    await flush();
    expect(h.session.getState().phase).toBe("blocked");
    expect(h.session.getState().caption).toBe("blocked: terminal not found");
    expect(h.timers.filter((t) => !t.cleared && !t.fired)).toEqual([]);
    expect(h.sockets).toHaveLength(0);
  });

  it("a seed cut short by a lost socket rejoins FULL (no stale from)", async () => {
    const h = setup();
    const s = await goLive(h);
    // A later seed on the same socket (server re-seed) is interrupted
    // after 2 of 50 bytes; the stale applied offset must not survive.
    s.pushText({ t: "seed", offset: 500, len: 50 });
    s.pushBinary(500, "ab");
    await flush();
    expect(h.session.getState().applied).toBe(0);
    s.closeFromServer(1006);
    h.fire(h.pendingTimer(250)?.ms ?? 250);
    await flush();
    expect(h.urls[1]).not.toContain("from=");
    const s2 = h.sockets[1];
    s2.pushText({ t: "hello", proto: "kb-term/1", session: "s1", epoch: "E1", harness: "omp", bound_sid: null });
    s2.pushText({ t: "seed", offset: 0, len: 3 });
    s2.pushBinary(0, "abc");
    await flush();
    expect(h.session.getState().applied).toBe(3);
    s2.pushBinary(3, "de");
    await flush();
    expect(h.session.getState().applied).toBe(5);
  });

  it("ticket 403 blocks with 'not permitted'", async () => {
    const h = setup();
    h.setMintError(new TermsApiError(403, "urn:kb:errors:terminal-intent-required", "Forbidden"));
    h.session.start();
    await flush();
    expect(h.session.getState().caption).toBe("blocked: not permitted");
  });

  it("close 1012 reconnects with backoff and a fresh ticket carrying from", async () => {
    const h = setup();
    const s = await goLive(h);
    s.closeFromServer(1012);
    expect(h.session.getState().phase).toBe("reconnecting");
    h.fire(250);
    await flush();
    expect(h.mintedTickets).toEqual(["tick-1", "tick-2"]);
    expect(h.urls[1]).toContain("from=105");
  });

  it("resize is applied inside the write queue, in order (write, resize, write)", async () => {
    const h = setup();
    const s = await goLive(h);
    h.calls.length = 0;
    s.pushBinary(105, "a");
    s.pushText({ t: "resize", seq: 1, offset: 106, cols: 80, rows: 24 });
    s.pushBinary(106, "b");
    await flush();
    expect(h.calls).toEqual([
      ["write", [97]],
      ["resize", 80, 24],
      ["write", [98]],
    ]);
  });

  it("unmount closes 1000, cancels timers and mints no further ticket", async () => {
    const h = setup();
    const s = await goLive(h);
    h.session.dispose();
    expect(s.closeCodes).toEqual([1000]);
    expect(h.timers.filter((t) => !t.cleared && !t.fired)).toEqual([]);
    for (const t of [...h.timers]) {
      if (!t.cleared) t.fn();
    }
    await flush();
    expect(h.mintedTickets).toEqual(["tick-1"]);
    s.closeFromServer(1000);
    await flush();
    expect(h.mintedTickets).toEqual(["tick-1"]);
  });

  it("sendInput queues nothing before live; it sends the JSON input frame once live", async () => {
    const h = setup();
    h.session.start();
    await flush();
    expect(h.session.sendInput("x")).toBe(false);
    expect(h.transients).toEqual(["input dropped (not live)"]);
    const s = h.sockets[0];
    s.pushText({ t: "hello", proto: "kb-term/1", session: "s1", epoch: "E1", harness: "omp", bound_sid: null });
    s.pushText(PROFILE);
    s.pushText({ t: "seed", offset: 0, len: 1 });
    s.pushBinary(0, "z");
    expect(h.session.sendInput("x")).toBe(true);
    expect(s.sent).toEqual(['{"t":"input","data":"x"}']);
  });

  it("the 1500ms first-frame watchdog fires only before the first applied frame", async () => {
    const h = setup();
    h.session.start();
    await flush();
    expect(h.pendingTimer(1500)).toBeDefined();
    h.fire(1500);
    expect(h.sockets[0].closeCodes).toEqual([1000]);
    await flush();
    expect(h.session.getState().phase).toBe("reconnecting");
  });

  it("a short binary frame is a protocol error and rejoins FULL", async () => {
    const h = setup();
    const s = await goLive(h);
    s.handlers.onMessageBinary(new Uint8Array([1, 2, 3]));
    expect(h.session.getState().gaps).toBe(1);
    expect(s.closeCodes).toEqual([1000]);
  });
  it("a gap payload is discarded: no write call for the gap bytes", async () => {
    const h = setup();
    const s = await goLive(h);
    const writesBefore = h.calls.filter((c) => c[0] === "write").length;
    s.pushBinary(999, "far");
    await flush();
    expect(h.calls.filter((c) => c[0] === "write")).toHaveLength(writesBefore);
  });

  it("a len-0 seed goes live with applied at the seed offset", async () => {
    const h = setup();
    h.session.start();
    await flush();
    const s = h.sockets[0];
    s.pushText({ t: "hello", proto: "kb-term/1", session: "s1", epoch: "E1", harness: "omp", bound_sid: null });
    s.pushText({ t: "seed", offset: 77, len: 0 });
    await flush();
    expect(h.session.getState().phase).toBe("live");
    expect(h.session.getState().applied).toBe(77);
    expect(h.session.getState().resets).toBe(1);
    expect(h.calls.filter((c) => c[0] === "reset")).toHaveLength(1);
  });

  it("malformed text is ignored; gfxreset and error frames are ignored", async () => {
    const h = setup();
    const s = await goLive(h);
    const writesBefore = h.calls.filter((c) => c[0] === "write").length;
    s.handlers.onMessageText("not json");
    s.pushText({ t: "gfxreset", seq: 9, gen: 2, offset: 105 });
    s.pushText({ t: "error", code: "boom", msg: "synthetic" });
    await flush();
    expect(h.session.getState().phase).toBe("live");
    expect(h.session.getState().applied).toBe(105);
    expect(h.calls.filter((c) => c[0] === "write")).toHaveLength(writesBefore);
  });

  it("exit with null code renders 'exited' without a code", async () => {
    const h = setup();
    const s = await goLive(h);
    s.pushText({ t: "exit", seq: 3, code: null, offset: 105 });
    expect(h.session.getState().caption).toBe("exited");
    expect(h.session.getState().exitCode).toBeNull();
  });

  it("binary input is UTF-8 decoded to the pinned string frame", async () => {
    const h = setup();
    const s = await goLive(h);
    expect(h.session.sendInput(new Uint8Array([26]))).toBe(true);
    expect(s.sent).toEqual(['{"t":"input","data":"\\u001a"}']);
    expect(s.sent[0]).not.toContain('"0":');
  });

  it("frames after exit are ignored and mint no further ticket", async () => {
    const h = setup();
    const s = await goLive(h);
    s.pushText({ t: "exit", seq: 3, code: 3, offset: 105 });
    const appliedBefore = h.session.getState().applied;
    s.handlers.onMessageBinary(new Uint8Array(0).fill(0, 0, 0));
    const fake = s.handlers;
    // In-order binary after exit must not advance applied.
    fake.onMessageBinary(new Uint8Array([0, 0, 0, 0, 0, 0, 0, 105, 120]));
    await flush();
    expect(h.session.getState().applied).toBe(appliedBefore);
    expect(h.mintedTickets).toEqual(["tick-1"]);
  });

  it("frames after dispose are ignored; the old socket close mints nothing", async () => {
    const h = setup();
    const s = await goLive(h);
    h.session.dispose();
    expect(s.closeCodes).toEqual([1000]);
    // In-flight frame on the same gen must not resurrect anything.
    s.handlers.onMessageBinary(new Uint8Array([0, 0, 0, 0, 0, 0, 0, 105, 120]));
    s.pushText({ t: "seed", offset: 0, len: 1 });
    await flush();
    expect(h.session.getState().phase).toBe("live");
    expect(h.mintedTickets).toEqual(["tick-1"]);
  });

  it("frames after block are ignored (no resurrect to live)", async () => {
    const h = setup();
    const s = await goLive(h);
    h.session.block("viewer build differs from session profile");
    expect(h.session.getState().phase).toBe("blocked");
    s.handlers.onMessageBinary(new Uint8Array([0, 0, 0, 0, 0, 0, 0, 105, 120]));
    await flush();
    expect(h.session.getState().phase).toBe("blocked");
    expect(h.session.getState().applied).toBe(105);
  });

  it("rejoinFull closes the old socket 1000, mints fresh without from, ignores the stale close", async () => {
    const h = setup();
    const s = await goLive(h);
    h.session.rejoinFull();
    await flush();
    expect(s.closeCodes).toEqual([1000]);
    expect(h.session.getState().applied).toBe(0);
    expect(h.mintedTickets).toEqual(["tick-1", "tick-2"]);
    const url = h.urls[h.urls.length - 1];
    expect(url).not.toContain("from=");
    expect(h.sockets).toHaveLength(2);
    // The stale old-socket close must mint nothing and schedule nothing.
    s.closeFromServer(1000);
    await flush();
    expect(h.mintedTickets).toEqual(["tick-1", "tick-2"]);
    expect(h.sockets).toHaveLength(2);
    expect(h.timers.filter((t) => !t.cleared && !t.fired && t.ms === 1500)).toHaveLength(1);
  });

  it("malformed resize/seed/profile do not poison the write queue", async () => {
    const h = setup();
    const s = await goLive(h);
    h.calls.length = 0;
    s.pushText({ t: "resize", seq: 1, offset: 105, cols: 0, rows: 24 });
    s.pushText({ t: "resize", seq: 2, offset: 105, cols: -1, rows: 24 });
    s.pushText({ t: "resize", seq: 3, offset: 105, cols: "80", rows: 24 });
    s.pushText({ t: "resize", seq: 4, offset: 105, rows: 24 });
    s.pushText({ t: "seed", offset: -5, len: 10 });
    s.pushText({ t: "seed", offset: 0, len: -1 });
    s.pushText({ t: "seed", offset: 2 ** 60, len: 3 });
    s.pushBinary(105, "ok");
    s.pushText({ t: "resize", seq: 5, offset: 107, cols: 80, rows: 24 });
    await flush();
    expect(h.calls).toEqual([
      ["write", [...new TextEncoder().encode("ok")]],
      ["resize", 80, 24],
    ]);
    expect(h.session.getState().applied).toBe(105 + 2);
  });

  it("a sink write throw does not starve later writes", async () => {
    const h = setup();
    const s = await goLive(h);
    // Replace the sink's write with one that throws once, then recovers.
    let throwsLeft = 1;
    const orig = h.sink.write.bind(h.sink);
    h.sink.write = (d, done) => {
      if (throwsLeft > 0) {
        throwsLeft -= 1;
        throw new Error("synthetic sink throw");
      }
      orig(d, done);
    };
    s.pushBinary(105, "a");
    s.pushBinary(106, "b");
    await flush();
    // One throw is swallowed; the second write still lands.
    expect(h.calls.filter((c) => c[0] === "write").map((c) => c[1])).toContainEqual([...new TextEncoder().encode("b")]);
  });
});

describe("nextDelay (T17)", () => {
  it("doubles from 250 ms, caps at 10 s, jitters within +/-20 percent", () => {
    const mid = () => 0.5;
    expect([0, 1, 2, 3, 4, 5, 6, 7, 20].map((a) => nextDelay(a, mid))).toEqual([
      250, 500, 1000, 2000, 4000, 8000, 10000, 10000, 10000,
    ]);
    for (let a = 0; a < 12; a += 1) {
      const base = Math.min(10_000, 250 * 2 ** a);
      expect(nextDelay(a, () => 0)).toBe(Math.round(base * 0.8));
      expect(nextDelay(a, () => 0.999999)).toBeLessThanOrEqual(Math.round(base * 1.2));
      expect(nextDelay(a, () => 0.999999)).toBeGreaterThan(base);
    }
  });

  it("a network-return trigger skips the pending backoff once", async () => {
    const h = setup();
    const s = await goLive(h);
    s.closeFromServer(1000);
    expect(h.session.getState().phase).toBe("reconnecting");
    const pending = h.pendingTimer(250);
    expect(pending).toBeDefined();
    const socketsBefore = h.sockets.length;
    h.session.kick();
    await flush();
    expect(pending?.cleared).toBe(true);
    expect(h.sockets.length).toBe(socketsBefore + 1);
    // The attempt still advanced: a flapping trigger cannot reset the backoff.
    expect(h.session.getState().attempt).toBe(1);
    // Nothing is waiting any more, so a second kick is a no-op.
    h.session.kick();
    await flush();
    expect(h.sockets.length).toBe(socketsBefore + 1);
  });

  it("kick leaves a live, ended or disposed session alone", async () => {
    const live = setup();
    await goLive(live);
    const n = live.sockets.length;
    live.session.kick();
    await flush();
    expect(live.sockets.length).toBe(n);

    const gone = setup();
    const gs = await goLive(gone);
    gs.closeFromServer(1000);
    gone.session.dispose();
    gone.session.kick();
    await flush();
    expect(gone.sockets.length).toBe(1);
  });
});
