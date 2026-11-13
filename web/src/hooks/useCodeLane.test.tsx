// @vitest-environment jsdom
//
// A5 — useCodeLane: the query is disabled without a code URL, retry is
// off (a failing cross-origin fetch must not burn a second attempt), the
// staleTime is finite (no-SSE-tie set, see the hook's header), and the
// key carries sid + repo.

import { afterEach, describe, expect, it, vi } from "vitest";
import type { ReactNode } from "react";
import { cleanup, renderHook, waitFor } from "@testing-library/react";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { CODE_LANE_STALE_MS, useCodeLane } from "./useCodeLane";

const CODE_URL = "https://code.example.test";

// Deliberately NO `retry: false` default here: a default that masks the
// hook's own `retry: false` would let a mutant that drops the hook option
// pass on the 1-call assertion alone.
function makeClient() {
  const qc = new QueryClient();
  const wrapper = ({ children }: { children: ReactNode }) => (
    <QueryClientProvider client={qc}>{children}</QueryClientProvider>
  );
  return { qc, wrapper };
}

afterEach(() => {
  cleanup();
  vi.unstubAllGlobals();
});

describe("useCodeLane", () => {
  it("disabled_without_a_code_url", async () => {
    const fetchFn = vi.fn(async () => new Response("{}", { status: 200 }));
    vi.stubGlobal("fetch", fetchFn);
    const { wrapper } = makeClient();
    const { result } = renderHook(() => useCodeLane(null, "sess-0001"), {
      wrapper,
    });
    await new Promise<void>((resolve) => setTimeout(resolve, 0));
    expect(fetchFn).not.toHaveBeenCalled();
    expect(result.current.data).toBeUndefined();
  });

  it("retry_is_off_and_stale_time_is_finite", async () => {
    // Finite by construction: a positive, sub-hour staleTime (and it is
    // the value the hook actually passes, not a copy).
    expect(CODE_LANE_STALE_MS).toBeGreaterThan(0);
    expect(CODE_LANE_STALE_MS).toBeLessThan(3_600_000);

    const fetchFn = vi.fn(async () => {
      throw new TypeError("Failed to fetch");
    });
    vi.stubGlobal("fetch", fetchFn);
    const { qc, wrapper } = makeClient();
    const { result } = renderHook(() => useCodeLane(CODE_URL, "sess-0001"), {
      wrapper,
    });
    await waitFor(() => expect(result.current.isError).toBe(true));
    // With no `retry: false` default on the test client, a hook that drops
    // its own `retry: false` retries (default 3 retries => 4 calls) and
    // fails this 1-call assertion.
    expect(fetchFn).toHaveBeenCalledTimes(1);
    // Pin the hook's own options directly: a mutant that sets
    // `staleTime: Infinity` while leaving the exported constant at 30_000
    // still yields 1 call yet is caught here.
    const entry = qc.getQueryCache().getAll()[0];
    expect(entry).toBeDefined();
    // TanStack stores the resolved options on the cache entry; read them
    // through a named const so the shape is visible at the assertion.
    const hookOptions = entry?.options as unknown as {
      staleTime: unknown;
      retry: unknown;
    };
    expect(hookOptions.staleTime).toBe(CODE_LANE_STALE_MS);
    expect(hookOptions.retry).toBe(false);
  });

  it("key_includes_sid_and_repo", async () => {
    const fetchFn = vi.fn(async (_input: string) =>
      new Response(JSON.stringify({ schema: "kbc-session-card/1" }), {
        status: 200,
        headers: { "Content-Type": "application/json" },
      }),
    );
    vi.stubGlobal("fetch", fetchFn);
    const { qc, wrapper } = makeClient();
    const { result, rerender } = renderHook(
      ({ sid, repo }: { sid: string; repo: string }) =>
        useCodeLane(CODE_URL, sid, repo),
      { wrapper, initialProps: { sid: "sess-0001", repo: "fixture" } },
    );
    await waitFor(() => expect(result.current.data).toBeDefined());
    expect(String(fetchFn.mock.calls[0][0])).toContain("sid=sess-0001");
    expect(String(fetchFn.mock.calls[0][0])).toContain("repo=fixture");
    expect(result.current.data?.schema).toBe("kbc-session-card/1");
    // The cache key itself carries sid + repo: a constant-key mutant
    // (["sessionCard", "code"]) passes the URL assertions above yet serves
    // stale data on a session switch.
    const cacheEntry = qc.getQueryCache().getAll()[0];
    expect(cacheEntry).toBeDefined();
    const cacheKey = cacheEntry?.queryKey as unknown;
    expect(cacheKey).toEqual(["sessionCard", "code", "sess-0001", "fixture"]);
    // A new sid must fetch again (a constant key would serve the cache and
    // never call fetch a second time).
    expect(fetchFn).toHaveBeenCalledTimes(1);
    rerender({ sid: "sess-0002", repo: "fixture" });
    await waitFor(() => expect(fetchFn).toHaveBeenCalledTimes(2));
    expect(String(fetchFn.mock.calls[1][0])).toContain("sid=sess-0002");
  });
});
