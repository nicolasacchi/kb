// @vitest-environment jsdom
// V76-R4d.2 — pin for the compare `dots` (three-dot) checkbox. No shipped
// spec exercises this control today; this is the same latent hazard round 2
// fixed in `Stacks.tsx` (stacks.spec.ts:93), closed prophylactically.
//
// react-router 7 wraps every navigation state update in
// React.startTransition, so a controlled checkbox bound DIRECTLY to
// `useSearchParams()` does not flip on the same tick as the change event:
// React's restoreControlledState snaps the DOM node back to the last
// committed render's value until the transition commits. The fix in
// `Compare.tsx` is local OPTIMISTIC state: the box flips synchronously and
// reconciles with the URL when the navigation lands. These assertions
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
import Compare from "./Compare";

(globalThis as { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = true;

let lastSearch = "";
function SearchSpy() {
  const [params] = useSearchParams();
  lastSearch = params.toString();
  return null;
}

function renderCompare() {
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
          { initialEntries: ["/r/demo/~compare"] },
          React.createElement(SearchSpy),
          React.createElement(
            Routes,
            null,
            React.createElement(Route, {
              path: "/r/:repo/~compare",
              element: React.createElement(Compare),
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

describe("Compare dots-toggle (react-router 7 startTransition, V76-R4d.2)", () => {
  beforeEach(() => {
    lastSearch = "";
    // `useRefs` is the only query that fires with no from/to; an empty ref
    // list is all the typeahead needs.
    globalThis.fetch = (async () =>
      new Response(JSON.stringify({ schema: "refs/1", repo: "demo", refs: [] }), {
        status: 200,
        headers: { "Content-Type": "application/json" },
      })) as typeof fetch;
  });

  afterEach(() => {
    document.body.innerHTML = "";
  });

  it("checkbox flips SYNCHRONOUSLY on change (no waitFor window)", () => {
    const { container } = renderCompare();
    const cb = container.querySelector<HTMLInputElement>("[data-kbc-compare-threedot]");
    expect(cb).toBeTruthy();
    expect(cb!.checked).toBe(false);
    // Native click OUTSIDE act() — the same gesture-and-read a spec's
    // check() performs. Pre-fix this assertion fails: the box stays
    // unchecked until the router's transition commits.
    cb!.click();
    expect(cb!.checked).toBe(true);
  });

  it("optimistic state reconciles with the URL once the navigation lands", async () => {
    const { container } = renderCompare();
    const cb = container.querySelector<HTMLInputElement>("[data-kbc-compare-threedot]")!;
    cb.click();
    expect(cb.checked).toBe(true);
    await settle();
    expect(lastSearch).toContain("dots=3");
    // After reconciliation the box is still checked — the effect that
    // re-reads the URL must not fight the optimistic value.
    expect(cb.checked).toBe(true);
  });
});
