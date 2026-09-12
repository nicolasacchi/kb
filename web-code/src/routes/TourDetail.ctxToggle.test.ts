// @vitest-environment jsdom
// V76-R4d.2 — pin for the tour detail `ctx` checkbox. No shipped spec
// exercises this control today; it carries the same latent hazard round 2
// fixed in `Stacks.tsx` (stacks.spec.ts:93), closed prophylactically.
//
// react-router 7 wraps every navigation state update in
// React.startTransition, so a controlled checkbox bound DIRECTLY to
// `useSearchParams()` does not flip on the same tick as the change event:
// React's restoreControlledState snaps the DOM node back to the last
// committed render's value until the transition commits. The fix in
// `TourDetail.tsx` is local OPTIMISTIC state: the box flips synchronously
// and reconciles with the URL when the navigation lands. These assertions
// therefore read the DOM IMMEDIATELY after a native click (no `waitFor`,
// no retry) — exactly the window the defect lives in.
//
// jsdom is opted into per-file (the suite default is `node`, deliberately):
// this defect only exists in the DOM commit path.
import { afterEach, beforeEach, describe, expect, it } from "vitest";
import * as React from "react";
import { createRoot, type Root } from "react-dom/client";
import { act } from "react-dom/test-utils";
import { MemoryRouter, Route, Routes, useSearchParams } from "react-router";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import TourDetail from "./TourDetail";

(globalThis as { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = true;

const TOUR_OUT = {
  schema: "tours/tour/1",
  repo: "demo",
  slug: "demo",
  title: "Demo tour",
  description_md: "",
  status: "active",
  authored_ref: null,
  revision: 1,
  content_hash: "abc",
  created_unix: 0,
  updated_unix: 0,
  steps: [
    {
      ordinal: 0,
      node: {
        id: "s1",
        kind: "note",
        title: "Step one",
        body_md: "hello",
        state: "present",
        reason: "resolved",
        address: "note:s1",
      },
      camera: null,
      ref: null,
    },
  ],
  honesty: {
    nodes: 1,
    edges: 0,
    steps: 1,
    pinned: 0,
    carried: 0,
    orphans: 0,
    present: 1,
    inert: 0,
    truncated_snippets: 0,
    stale_pins: 0,
    live_queries: false,
    budget: { max_nodes: 200, max_edges: 400, max_snippet_lines: 40 },
    notes: [],
  },
};

let lastSearch = "";
function SearchSpy() {
  const [params] = useSearchParams();
  lastSearch = params.toString();
  return null;
}

function renderTourDetail() {
  const query = new QueryClient({ defaultOptions: { queries: { retry: false } } });
  const container = document.createElement("div");
  document.body.appendChild(container);
  const root: Root = createRoot(container);
  act(() => {
    root.render(
      React.createElement(
        QueryClientProvider,
        { client: query },
        React.createElement(
          MemoryRouter,
          { initialEntries: ["/r/demo/~tours/demo"] },
          React.createElement(SearchSpy),
          React.createElement(
            Routes,
            null,
            React.createElement(Route, {
              path: "/r/:repo/~tours/:slug",
              element: React.createElement(TourDetail),
            }),
          ),
        ),
      ),
    );
  });
  return { container, root };
}

async function settle() {
  for (let i = 0; i < 20; i++) {
    await act(async () => {
      await new Promise((r) => setTimeout(r, 0));
    });
  }
}

describe("TourDetail ctx-toggle (react-router 7 startTransition, V76-R4d.2)", () => {
  beforeEach(() => {
    lastSearch = "";
    globalThis.fetch = (async (input: RequestInfo | URL) => {
      const url = String(input);
      const body = url.includes("/api/repos")
        ? { schema: "repos/1", repos: [], loopback: false }
        : TOUR_OUT;
      return new Response(JSON.stringify(body), {
        status: 200,
        headers: { "Content-Type": "application/json" },
      });
    }) as typeof fetch;
  });

  afterEach(() => {
    document.body.innerHTML = "";
  });

  it("checkbox flips SYNCHRONOUSLY on change (no waitFor window)", async () => {
    const { container } = renderTourDetail();
    // The ctx toggle renders only once the tour query lands.
    await settle();
    const cb = container.querySelector<HTMLInputElement>("[data-kbc-tour-ctx]");
    expect(cb).toBeTruthy();
    expect(cb!.checked).toBe(false);
    // Native click OUTSIDE act() — the same gesture-and-read a spec's
    // check() performs. Pre-fix this assertion fails: the box stays
    // unchecked until the router's transition commits.
    cb!.click();
    expect(cb!.checked).toBe(true);
  });

  it("optimistic state reconciles with the URL once the navigation lands", async () => {
    const { container } = renderTourDetail();
    await settle();
    const cb = container.querySelector<HTMLInputElement>("[data-kbc-tour-ctx]")!;
    cb.click();
    expect(cb.checked).toBe(true);
    await settle();
    expect(lastSearch).toContain("ctx=1");
    // After reconciliation the box is still checked — the effect that
    // re-reads the URL must not fight the optimistic value.
    expect(cb.checked).toBe(true);
  });
});
