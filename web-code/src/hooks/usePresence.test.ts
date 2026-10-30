// @vitest-environment jsdom
import { afterEach, describe, expect, it, vi } from "vitest";
import { createElement } from "react";
import { createRoot, type Root } from "react-dom/client";
import { act } from "react-dom/test-utils";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import {
  PRESENCE_POLL_MS,
  presenceEnabled,
  presenceKey,
  presenceQueryOptions,
  usePresence,
} from "./usePresence";

(globalThis as { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = true;

describe("presence polling contract (OL PRES3)", () => {
  it("keys under [presence, repo] and polls every 10 s with a matching staleTime", () => {
    expect(presenceKey("r")).toEqual(["presence", "r"]);
    const o = presenceQueryOptions("r", true);
    expect(PRESENCE_POLL_MS).toBe(10_000);
    expect(o.refetchInterval).toBe(10_000);
    expect(o.staleTime).toBe(10_000);
    expect("refetchIntervalInBackground" in o).toBe(false);
  });

  it("is enabled only with the pref on, a visible document and a repo", () => {
    expect(presenceEnabled({ pref: true, visible: true, repo: "r" })).toBe(true);
    expect(presenceEnabled({ pref: false, visible: true, repo: "r" })).toBe(false);
    expect(presenceEnabled({ pref: true, visible: false, repo: "r" })).toBe(false);
    expect(presenceEnabled({ pref: true, visible: true, repo: undefined })).toBe(false);
  });
});

describe("usePresence", () => {
  let root: Root | null = null;
  afterEach(() => {
    act(() => root?.unmount());
    root = null;
    delete (document as unknown as Record<string, unknown>).visibilityState;
    vi.unstubAllGlobals();
    vi.restoreAllMocks();
  });

  function mount(enabled: boolean, hidden = false) {
    const fetchSpy = vi.fn(async () => new Response(
      JSON.stringify({ schema: "kbc-presence/1", available: true, now_unix: 0, files: [] }),
      { status: 200, headers: { "Content-Type": "application/json" } },
    ));
    vi.stubGlobal("fetch", fetchSpy);
    Object.defineProperty(document, "visibilityState", {
      configurable: true,
      get: () => (hidden ? "hidden" : "visible"),
    });
    function Probe(): null {
      usePresence("r", enabled);
      return null;
    }
    const client = new QueryClient({ defaultOptions: { queries: { retry: false } } });
    const el = document.createElement("div");
    root = createRoot(el);
    act(() => {
      root!.render(createElement(QueryClientProvider, { client }, createElement(Probe)));
    });
    return fetchSpy;
  }

  it("fetches once when on and visible", async () => {
    const spy = mount(true);
    await act(async () => {
      await Promise.resolve();
    });
    expect(spy).toHaveBeenCalledTimes(1);
    expect(String((spy.mock.calls[0] as unknown[])[0])).toBe("/api/kb/presence?repo=r");
  });

  it("makes zero requests when the pref is off", async () => {
    const spy = mount(false);
    await act(async () => {
      await Promise.resolve();
    });
    expect(spy).not.toHaveBeenCalled();
  });

  it("makes zero requests while the document is hidden", async () => {
    const spy = mount(true, true);
    await act(async () => {
      await Promise.resolve();
    });
    expect(spy).not.toHaveBeenCalled();
  });
});
