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

function wrapper() {
  const qc = new QueryClient({
    defaultOptions: { queries: { retry: false } },
  });
  return ({ children }: { children: ReactNode }) => (
    <QueryClientProvider client={qc}>{children}</QueryClientProvider>
  );
}

afterEach(() => {
  cleanup();
  vi.unstubAllGlobals();
});

describe("useCodeLane", () => {
  it("disabled_without_a_code_url", async () => {
    const fetchFn = vi.fn(async () => new Response("{}", { status: 200 }));
    vi.stubGlobal("fetch", fetchFn);
    const { result } = renderHook(() => useCodeLane(null, "sess-0001"), {
      wrapper: wrapper(),
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
    const { result } = renderHook(() => useCodeLane(CODE_URL, "sess-0001"), {
      wrapper: wrapper(),
    });
    await waitFor(() => expect(result.current.isError).toBe(true));
    expect(fetchFn).toHaveBeenCalledTimes(1);
  });

  it("key_includes_sid_and_repo", async () => {
    const fetchFn = vi.fn(async (_input: string) =>
      new Response(JSON.stringify({ schema: "kbc-session-card/1" }), {
        status: 200,
        headers: { "Content-Type": "application/json" },
      }),
    );
    vi.stubGlobal("fetch", fetchFn);
    const { result } = renderHook(() => useCodeLane(CODE_URL, "sess-0001", "fixture"), {
      wrapper: wrapper(),
    });
    await waitFor(() => expect(result.current.data).toBeDefined());
    expect(String(fetchFn.mock.calls[0][0])).toContain("sid=sess-0001");
    expect(String(fetchFn.mock.calls[0][0])).toContain("repo=fixture");
    expect(result.current.data?.schema).toBe("kbc-session-card/1");
  });
});
