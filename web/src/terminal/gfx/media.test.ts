import { describe, expect, it } from "vitest";
import {
  FAILURE_COOLDOWN_MS,
  idsToPrefetch,
  MAX_CONCURRENT_FETCHES,
  MEDIA_BUDGET_BYTES,
  MEDIA_BUDGET_COMPACT_BYTES,
  MediaCache,
  NETWORK_RETRY_DELAYS_MS,
  prefetchWindow,
} from "./media";

const sha = (n: number): string => n.toString(16).padStart(64, "0");

interface Harness {
  cache: MediaCache;
  calls: Array<{ url: string; headers: Record<string, string>; credentials?: string }>;
  revoked: string[];
  sleeps: number[];
  clock: { t: number };
}

function harness(
  responder: (url: string, call: number) => Promise<Response> | Response,
  opts: { compact?: boolean; size?: number; decodeFails?: boolean } = {},
): Harness {
  const calls: Harness["calls"] = [];
  const revoked: string[] = [];
  const sleeps: number[] = [];
  let minted = 0;
  const clock = { t: 0 };
  const fetchImpl = ((input: RequestInfo | URL, init?: RequestInit) => {
    calls.push({
      url: String(input),
      headers: init?.headers as Record<string, string>,
      credentials: init?.credentials,
    });
    return Promise.resolve(responder(String(input), calls.length));
  }) as typeof fetch;
  const cache = new MediaCache({
    compact: opts.compact,
    fetchImpl,
    createObjectURL: () => {
      minted += 1;
      return `blob:test/${minted}`;
    },
    revokeObjectURL: (u) => {
      revoked.push(u);
    },
    decode: () => (opts.decodeFails === true ? Promise.reject(new Error("bad")) : Promise.resolve()),
    sleep: (ms) => {
      sleeps.push(ms);
      return Promise.resolve();
    },
    now: () => clock.t,
  });
  return { cache, calls, revoked, sleeps, clock };
}

const png = (n = 8): Response => new Response(new Uint8Array(n), { status: 200 });

/** A 200 whose blob only CLAIMS `size` bytes (no real allocation). */
function bigResponse(size: number): Response {
  return { ok: true, status: 200, blob: () => Promise.resolve({ size } as Blob) } as Response;
}

describe("MediaCache", () => {
  it("sends the intent header", async () => {
    const h = harness(() => png());
    const out = await h.cache.load(sha(1));
    expect(out).not.toBe("missing");
    expect(h.calls).toHaveLength(1);
    expect(h.calls[0].url).toBe(`/api/terms/media/${sha(1)}`);
    expect(h.calls[0].headers["X-Kb-Intent"]).toBe("terminal");
    expect(h.calls[0].credentials).toBe("same-origin");
  });

  it("refuses a key that is not hex64 without a request", async () => {
    const h = harness(() => png());
    expect(await h.cache.load("../etc/passwd")).toBe("missing");
    expect(await h.cache.load("A".repeat(64))).toBe("missing");
    expect(h.calls).toHaveLength(0);
  });

  it("dedupes concurrent loads of one sha", async () => {
    const h = harness(() => png());
    const [a, b, c] = await Promise.all([
      h.cache.load(sha(1)),
      h.cache.load(sha(1)),
      h.cache.load(sha(1)),
    ]);
    expect(h.calls).toHaveLength(1);
    expect(a).toEqual(b);
    expect(b).toEqual(c);
    // A later call is a cache hit: still one request.
    await h.cache.load(sha(1));
    expect(h.calls).toHaveLength(1);
  });

  it("caps concurrency at 4", async () => {
    expect(MAX_CONCURRENT_FETCHES).toBe(4);
    let running = 0;
    let peak = 0;
    const waiters: Array<() => void> = [];
    const h = harness(
      () =>
        new Promise<Response>((resolve) => {
          running += 1;
          peak = Math.max(peak, running);
          waiters.push(() => {
            running -= 1;
            resolve(png());
          });
        }),
    );
    const all = Array.from({ length: 10 }, (_, i) => h.cache.load(sha(i + 1)));
    // Let the first wave start; only four may be running.
    await new Promise((r) => setTimeout(r, 10));
    expect(running).toBe(4);
    while (waiters.length > 0 || running > 0) {
      const w = waiters.shift();
      if (w !== undefined) w();
      await new Promise((r) => setTimeout(r, 1));
    }
    await Promise.all(all);
    expect(peak).toBe(4);
    expect(h.calls).toHaveLength(10);
  });

  it("404 is missing and sticky", async () => {
    const h = harness(() => new Response(null, { status: 404 }));
    expect(await h.cache.load(sha(1))).toBe("missing");
    expect(h.cache.peek(sha(1))).toBe("missing");
    expect(await h.cache.load(sha(1))).toBe("missing");
    expect(h.calls).toHaveLength(1);
    // A 404 is not retried either.
    expect(h.sleeps).toEqual([]);
    // forget() lets the next load ask again.
    h.cache.forget(sha(1));
    expect(h.cache.peek(sha(1))).toBeUndefined();
    await h.cache.load(sha(1));
    expect(h.calls).toHaveLength(2);
  });

  it("retries network errors with backoff then missing", async () => {
    const h = harness(() => Promise.reject(new TypeError("network")));
    expect(await h.cache.load(sha(1))).toBe("missing");
    expect(h.sleeps).toEqual([...NETWORK_RETRY_DELAYS_MS]);
    expect(h.sleeps).toEqual([200, 400, 800]);
    // One first try plus three retries.
    expect(h.calls).toHaveLength(4);
    // The failure is shown as missing but NOT sticky: inside the cooldown no
    // request is made, after it a later load asks the network again.
    expect(h.cache.peek(sha(1))).toBe("missing");
    await h.cache.load(sha(1));
    expect(h.calls).toHaveLength(4);
    h.clock.t = FAILURE_COOLDOWN_MS;
    await h.cache.load(sha(1));
    expect(h.calls).toHaveLength(8);
  });

  it("a transient failure that recovers loads the picture", async () => {
    const h = harness((_u, n) => (n < 3 ? new Response(null, { status: 503 }) : png()));
    const out = await h.cache.load(sha(1));
    expect(out).not.toBe("missing");
    expect(h.sleeps).toEqual([200, 400]);
  });

  it("a gate refusal is missing without retry or stickiness", async () => {
    const h = harness(() => new Response(null, { status: 403 }));
    expect(await h.cache.load(sha(1))).toBe("missing");
    expect(h.sleeps).toEqual([]);
    // Shown as missing, but not sticky (retried after the cooldown).
    expect(h.cache.peek(sha(1))).toBe("missing");
    h.clock.t = FAILURE_COOLDOWN_MS;
    await h.cache.load(sha(1));
    expect(h.calls).toHaveLength(2);
  });

  it("undecodable bytes are missing, sticky, and their url is revoked", async () => {
    const h = harness(() => png(), { decodeFails: true });
    expect(await h.cache.load(sha(1))).toBe("missing");
    expect(h.revoked).toEqual(["blob:test/1"]);
    expect(h.cache.peek(sha(1))).toBe("missing");
    expect(h.cache.size).toBe(0);
  });

  it("evicts LRU and revokes URLs but never a pinned one", async () => {
    const mib = 1024 * 1024;
    const h = harness(() => bigResponse(40 * mib));
    expect(await h.cache.load(sha(1))).not.toBe("missing");
    expect(await h.cache.load(sha(2))).not.toBe("missing");
    expect(h.cache.size).toBe(2);
    expect(h.cache.usedBytes).toBe(80 * mib);
    // Touch #1 so #2 becomes the least recently used, then pin #1.
    await h.cache.load(sha(1));
    h.cache.pin(sha(1));
    // 3 x 40 MiB > 96 MiB: exactly one eviction, the LRU unpinned (#2).
    await h.cache.load(sha(3));
    expect(h.cache.peek(sha(2))).toBeUndefined();
    expect(h.revoked).toEqual(["blob:test/2"]);
    expect(h.cache.size).toBe(2);
    // A fourth: #1 is pinned, so #3 (older than #4) goes instead.
    await h.cache.load(sha(4));
    expect(h.cache.peek(sha(1))).not.toBeUndefined();
    expect(h.cache.peek(sha(3))).toBeUndefined();
    expect(h.revoked).toEqual(["blob:test/2", "blob:test/3"]);
    // A miss after eviction reloads.
    const before = h.calls.length;
    await h.cache.load(sha(2));
    expect(h.calls.length).toBe(before + 1);
    expect(h.revoked).toContain("blob:test/4");
    expect(h.cache.peek(sha(1))).not.toBeUndefined();
    // Pins are refcounted: one unpin keeps it pinned, the second releases.
    h.cache.pin(sha(1));
    h.cache.unpin(sha(1));
    h.cache.unpin(sha(1));
    expect(h.cache.usedBytes).toBeLessThanOrEqual(h.cache.budgetBytes);
    // forgetAll revokes everything unpinned and keeps what is pinned.
    h.cache.pin(sha(2));
    h.cache.forgetAll();
    expect(h.cache.peek(sha(2))).not.toBeUndefined();
    expect(h.cache.peek(sha(1))).toBeUndefined();
    expect(h.cache.size).toBe(1);
  });

  it("compact budget is smaller", () => {
    expect(MEDIA_BUDGET_BYTES).toBe(96 * 1024 * 1024);
    expect(MEDIA_BUDGET_COMPACT_BYTES).toBe(32 * 1024 * 1024);
    expect(MEDIA_BUDGET_COMPACT_BYTES).toBeLessThan(MEDIA_BUDGET_BYTES);
    expect(harness(() => png()).cache.budgetBytes).toBe(MEDIA_BUDGET_BYTES);
    expect(harness(() => png(), { compact: true }).cache.budgetBytes).toBe(
      MEDIA_BUDGET_COMPACT_BYTES,
    );
  });

  it("notifies listeners when a load settles", async () => {
    const h = harness(() => png());
    let n = 0;
    h.cache.onChange(() => {
      n += 1;
    });
    await h.cache.load(sha(1));
    expect(n).toBe(1);
  });
});

describe("prefetch window", () => {
  it("idsToPrefetch picks viewport plus one screen each side and live defs only", () => {
    // 30-row screen at viewportY 100: rows 70 .. 160 (one screen above the
    // viewport, the viewport itself, and one screen below it).
    expect(prefetchWindow(100, 30, 1000)).toEqual({ start: 70, count: 90 });
    expect(prefetchWindow(10, 30, 1000)).toEqual({ start: 0, count: 70 });
    expect(prefetchWindow(990, 30, 1000)).toEqual({ start: 960, count: 40 });
    expect(prefetchWindow(0, 30, 0)).toEqual({ start: 0, count: 0 });

    const cells = [{ id: 5 }, { id: 5 }, { id: 9 }, { id: 5 }, { id: 2 }];
    // Ids with cells in the window come first, in order, deduplicated;
    // live defs follow (ascending) unless already listed. A seed-batch
    // def with no cell in the window (id 77) is not in the result.
    expect(idsToPrefetch(cells, [30, 9, 12])).toEqual([5, 9, 2, 12, 30]);
    expect(idsToPrefetch([], [])).toEqual([]);
    expect(idsToPrefetch([], new Set([4]))).toEqual([4]);
    expect(idsToPrefetch(cells, [])).not.toContain(77);
  });
});

describe("MediaCache.dispose (T15 r1 M3)", () => {
  it("revokes URLs of loads settling after dispose and starts no queued fetch", async () => {
    const releases: Array<() => void> = [];
    const h = harness(
      () =>
        new Promise<Response>((res) => {
          releases.push(() => res(png()));
        }),
    );
    const loads = [1, 2, 3, 4, 5].map((n) => h.cache.load(sha(n)));
    await Promise.resolve();
    await Promise.resolve();
    expect(h.calls).toHaveLength(MAX_CONCURRENT_FETCHES);
    h.cache.dispose();
    for (const r of releases) r();
    const outs = await Promise.all(loads);
    expect(outs.every((o) => o === "missing")).toBe(true);
    expect(h.cache.size).toBe(0);
    expect(h.calls).toHaveLength(MAX_CONCURRENT_FETCHES);
    expect(h.cache.usedBytes).toBe(0);
  });
});

describe("MediaCache transient failures (T15 r1 M2)", () => {
  it("peek reports missing after giving up and load retries after the cooldown", async () => {
    let clock = 0;
    let n = 0;
    const cache = new MediaCache({
      fetchImpl: (() => {
        n += 1;
        return Promise.resolve(n <= 4 ? new Response(null, { status: 503 }) : png());
      }) as typeof fetch,
      createObjectURL: () => "blob:x",
      revokeObjectURL: () => {},
      decode: () => Promise.resolve(),
      sleep: () => Promise.resolve(),
      now: () => clock,
    });
    expect(await cache.load(sha(1))).toBe("missing");
    expect(cache.peek(sha(1))).toBe("missing");
    expect(await cache.load(sha(1))).toBe("missing");
    expect(n).toBe(4);
    clock = FAILURE_COOLDOWN_MS;
    expect(await cache.load(sha(1))).not.toBe("missing");
    expect(n).toBe(5);
  });
});
