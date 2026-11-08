// @vitest-environment jsdom
//
// A4 — useSessionCard: the sid-less mount (the page calls the hook before
// its `!sid` early return) must not fire `GET /api/sessions//card`.
import { afterEach, describe, expect, it, vi } from "vitest";
import type { ReactNode } from "react";
import { cleanup, renderHook, waitFor } from "@testing-library/react";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { useSessionCard } from "./useSessionCard";

const SID = "abcdef0123456789";

function card(sid: string) {
  return { schema: "kb-session-card/1", session_id: sid, kb: "demo" };
}

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

describe("useSessionCard", () => {
  it("never fetches for an empty sid", async () => {
    const fetchFn = vi.fn(async () => ({
      ok: true,
      status: 200,
      statusText: "OK",
      json: async () => card(""),
    }));
    vi.stubGlobal("fetch", fetchFn);
    const { result } = renderHook(() => useSessionCard(""), {
      wrapper: wrapper(),
    });
    // Give the disabled query a tick: a fetch would be issued here.
    const { promise, resolve } = Promise.withResolvers<void>();
    setTimeout(resolve, 0);
    await promise;
    expect(fetchFn).not.toHaveBeenCalled();
    expect(result.current.card).toBeNull();
  });

  it("fetches the card for a sid", async () => {
    const fetchFn = vi.fn(async (url: unknown) => ({
      ok: true,
      status: 200,
      statusText: "OK",
      json: async () => card(SID),
    }));
    vi.stubGlobal("fetch", fetchFn);
    const { result } = renderHook(() => useSessionCard(SID), {
      wrapper: wrapper(),
    });
    await waitFor(() => expect(fetchFn).toHaveBeenCalledTimes(1));
    expect(String(fetchFn.mock.calls[0][0])).toContain(`/api/sessions/${SID}/card`);
    await waitFor(() => expect(result.current.card).not.toBeNull());
    expect(result.current.card?.session_id).toBe(SID);
  });
});
