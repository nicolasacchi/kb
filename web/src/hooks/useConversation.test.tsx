// @vitest-environment jsdom
import { afterEach, describe, expect, it, vi } from "vitest";
import type { ReactNode } from "react";
import { cleanup, renderHook, waitFor } from "@testing-library/react";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { conversationKey, mergeUnchanged, useConversation } from "./useConversation";
import type { ConversationResult } from "../api/sessions";

const SID = "sid-1";
const conv = (gen: string, rows: number, unchanged = false) => ({
  schema: "conversation/1",
  total: rows,
  from: rows ? 1 : null,
  next_cursor: null,
  capture_gen: gen,
  unchanged,
  rows: Array.from({ length: rows }, (_, i) => ({
    n: i + 1,
    id: `t-${i}`,
    ts: null,
    prompt: `p${i}`,
    answer: null,
    asks: [],
    commits: [],
    files_touched: 0,
    errors: 0,
    subagents: { n: 0, errors: 0 },
  })),
});
const ok = (body: unknown) => ({ ok: true, status: 200, json: async () => ({ kb: "demo", conversation: body }) });

function setup() {
  const qc = new QueryClient({ defaultOptions: { queries: { retry: false } } });
  const wrapper = ({ children }: { children: ReactNode }) => (
    <QueryClientProvider client={qc}>{children}</QueryClientProvider>
  );
  return { qc, wrapper };
}

afterEach(() => {
  cleanup();
  vi.unstubAllGlobals();
});

describe("useConversation", () => {
  it("key shape", () => expect(conversationKey("s")).toEqual(["sessions", "conversation", "s"]));

  it("does not fetch without a sid", async () => {
    const f = vi.fn();
    vi.stubGlobal("fetch", f);
    const { wrapper } = setup();
    renderHook(() => useConversation(undefined), { wrapper });
    await new Promise<void>((r) => setTimeout(r, 0));
    expect(f).not.toHaveBeenCalled();
  });

  it("requests the projection, and re-asks with the held conv_gen, keeping rows on unchanged", async () => {
    const f = vi.fn(async (_u: unknown) => ok(conv("100-2", 2)));
    vi.stubGlobal("fetch", f);
    const { qc, wrapper } = setup();
    const { result } = renderHook(() => useConversation(SID), { wrapper });
    await waitFor(() => expect(result.current.data?.kind).toBe("ok"));
    const first = String(f.mock.calls[0]?.[0]);
    expect(first).toContain(`/api/sessions/${SID}/view?`);
    expect(first).toContain("fields=conversation");
    expect(first).not.toContain("conv_gen");

    f.mockImplementation(async () => ok(conv("100-2", 0, true)));
    await qc.invalidateQueries({ queryKey: conversationKey(SID) });
    await waitFor(() => expect(f).toHaveBeenCalledTimes(2));
    expect(String(f.mock.calls[1]?.[0])).toContain("conv_gen=100-2");
    await waitFor(() => {
      const d = qc.getQueryData<ConversationResult>(conversationKey(SID));
      expect(d?.kind === "ok" && d.conversation.rows.length).toBe(2);
    });
  });

  it("a 404 resolves to none, not an error", async () => {
    vi.stubGlobal("fetch", vi.fn(async () => ({ ok: false, status: 404, json: async () => ({}) })));
    const { wrapper } = setup();
    const { result } = renderHook(() => useConversation(SID), { wrapper });
    await waitFor(() => expect(result.current.data).toEqual({ kind: "none" }));
    expect(result.current.isError).toBe(false);
  });

  it("another non-ok status is an error", async () => {
    vi.stubGlobal("fetch", vi.fn(async () => ({ ok: false, status: 500, json: async () => ({}) })));
    const { wrapper } = setup();
    const { result } = renderHook(() => useConversation(SID), { wrapper });
    await waitFor(() => expect(result.current.isError).toBe(true));
  });
});

describe("mergeUnchanged", () => {
  it("a changed answer replaces what is held", () => {
    const held: ConversationResult = { kind: "ok", kb: "d", conversation: conv("1-1", 1) };
    const next: ConversationResult = { kind: "ok", kb: "d", conversation: conv("2-2", 2) };
    expect(mergeUnchanged(held, next)).toBe(next);
  });
});
