// @vitest-environment jsdom
// V76-R4d round 2 — pin for the stacks `all` toggle (stacks.spec.ts:93).
//
// react-router 7 wraps every navigation state update in
// React.startTransition, so a controlled checkbox bound DIRECTLY to
// `useSearchParams()` does not flip on the same tick as the change event:
// React's restoreControlledState snaps the DOM node back to the last
// committed render's value until the transition commits, and Playwright's
// `check()` — which reads the DOM state immediately after the click — saw
// "Clicking the checkbox did not change its state".
//
// The fix in `Stacks.tsx` is local OPTIMISTIC state: the box flips
// synchronously and reconciles with the URL when the navigation lands.
// These assertions therefore read the DOM IMMEDIATELY after a native click
// (no `waitFor`, no retry) — exactly the window the spec lost.
//
// jsdom is opted into per-file (the suite default is `node`, deliberately):
// this defect only exists in the DOM commit path.
import { afterEach, beforeEach, describe, expect, it } from "vitest";
import * as React from "react";
import { createRoot, type Root } from "react-dom/client";
import { act } from "react-dom/test-utils";
import { MemoryRouter, Route, Routes, useSearchParams } from "react-router";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import Stacks from "./Stacks";

(globalThis as { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = true;

const STACKS_EMPTY = {
  schema: "stacks/1",
  repo: "demo",
  default_branch: "main",
  stacks: [],
  truncated: false,
};

let lastSearch = "";
function SearchSpy() {
  const [params] = useSearchParams();
  lastSearch = params.toString();
  return null;
}

function renderStacks() {
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
          { initialEntries: ["/r/demo/~stacks"] },
          React.createElement(SearchSpy),
          React.createElement(
            Routes,
            null,
            React.createElement(Route, {
              path: "/r/:repo/~stacks",
              element: React.createElement(Stacks),
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

describe("Stacks all-toggle (react-router 7 startTransition, V76-R4d round 2)", () => {
  beforeEach(() => {
    lastSearch = "";
    globalThis.fetch = (async () =>
      new Response(JSON.stringify(STACKS_EMPTY), {
        status: 200,
        headers: { "Content-Type": "application/json" },
      })) as typeof fetch;
  });

  afterEach(() => {
    document.body.innerHTML = "";
  });

  it("checkbox flips SYNCHRONOUSLY on change (no waitFor window)", () => {
    const { container } = renderStacks();
    const cb = container.querySelector<HTMLInputElement>("[data-kbc-stacks-all]");
    expect(cb).toBeTruthy();
    expect(cb!.checked).toBe(false);
    // Native click OUTSIDE act() — the same gesture-and-read Playwright's
    // check() performs. Pre-fix this assertion fails: the box stays
    // unchecked until the router's transition commits.
    cb!.click();
    expect(cb!.checked).toBe(true);
  });

  it("optimistic state reconciles with the URL once the navigation lands", async () => {
    const { container } = renderStacks();
    const cb = container.querySelector<HTMLInputElement>("[data-kbc-stacks-all]")!;
    cb.click();
    expect(cb.checked).toBe(true);
    await settle();
    expect(lastSearch).toContain("all=1");
    // After reconciliation the box is still checked — the effect that
    // re-reads the URL must not fight the optimistic value.
    expect(cb.checked).toBe(true);
  });
});
