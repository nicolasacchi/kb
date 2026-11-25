// @vitest-environment jsdom
import { afterEach, describe, expect, it, vi } from "vitest";
import { renderHook, waitFor, cleanup } from "@testing-library/react";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import type { ReactNode } from "react";
import { knowledgeKey, knowledgeSearchKey, useKnowledgeRecalls, useKnowledgeResearch } from "./useKnowledge";

function wrap() {
  const qc = new QueryClient({ defaultOptions: { queries: { retry: false } } });
  const W = ({ children }: { children: ReactNode }) => <QueryClientProvider client={qc}>{children}</QueryClientProvider>;
  return { qc, W };
}
afterEach(() => {
  cleanup();
  vi.unstubAllGlobals();
});

describe("useKnowledge", () => {
  it("key shapes", () => {
    expect(knowledgeKey("recalls", "s1")).toEqual(["sessions", "knowledge", "recalls", "s1"]);
    expect(knowledgeSearchKey("project", "q")).toEqual(["memories", "rail", "project", "q"]);
  });
  it("is disabled without a sid", () => {
    const f = vi.fn();
    vi.stubGlobal("fetch", f);
    const { W } = wrap();
    const { result } = renderHook(() => useKnowledgeRecalls(undefined), { wrapper: W });
    expect(result.current.fetchStatus).toBe("idle");
    expect(f).not.toHaveBeenCalled();
  });
  it("a [sessions] invalidate refetches the lanes of the sid", async () => {
    const f = vi.fn(async () => ({ ok: true, status: 200, statusText: "", json: async () => ({ recalls: [] }) }));
    vi.stubGlobal("fetch", f);
    const { qc, W } = wrap();
    const { result } = renderHook(() => [useKnowledgeRecalls("s1"), useKnowledgeResearch("s1")] as const, { wrapper: W });
    await waitFor(() => expect(f).toHaveBeenCalledTimes(2));
    await qc.invalidateQueries({ queryKey: ["sessions"] });
    await waitFor(() => expect(f).toHaveBeenCalledTimes(4));
    expect(result.current[0].isError).toBe(false);
  });
  it("does not refetch on its own", async () => {
    const f = vi.fn(async () => ({ ok: true, status: 200, statusText: "", json: async () => ({ recalls: [] }) }));
    vi.stubGlobal("fetch", f);
    const { W } = wrap();
    const { rerender } = renderHook(() => useKnowledgeRecalls("s1"), { wrapper: W });
    await waitFor(() => expect(f).toHaveBeenCalledTimes(1));
    rerender();
    expect(f).toHaveBeenCalledTimes(1);
  });
});
