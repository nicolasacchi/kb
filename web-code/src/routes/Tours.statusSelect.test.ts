// @vitest-environment jsdom
// V76-R4d.2 — pin for the tours list `status` <select>. No shipped spec
// exercises this control today; it is the select flavour of the same latent
// hazard round 2 fixed in `Stacks.tsx` (stacks.spec.ts:93), closed
// prophylactically.
//
// react-router 7 wraps every navigation state update in
// React.startTransition, so a controlled `value=` bound DIRECTLY to
// `useSearchParams()` does not change on the same tick as the change event:
// React's restoreControlledState snaps the DOM node back to the last
// committed render's value until the transition commits. The fix in
// `Tours.tsx` is local OPTIMISTIC state: the select flips synchronously
// and reconciles with the URL when the navigation lands. These assertions
// therefore read the DOM IMMEDIATELY after a native change event (no
// `waitFor`, no retry) — exactly the window the defect lives in.
//
// jsdom is opted into per-file (the suite default is `node`, deliberately):
// this defect only exists in the DOM commit path.
import { afterEach, beforeEach, describe, expect, it } from "vitest";
import * as React from "react";
import { createRoot, type Root } from "react-dom/client";
import { act } from "react-dom/test-utils";
import { MemoryRouter, Route, Routes, useSearchParams } from "react-router";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import Tours from "./Tours";

(globalThis as { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = true;

const TOURS_LIST = {
  schema: "tours/1",
  repo: "demo",
  statuses_available: ["draft", "active"],
  tours: [],
};

let lastSearch = "";
function SearchSpy() {
  const [params] = useSearchParams();
  lastSearch = params.toString();
  return null;
}

function renderTours() {
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
          { initialEntries: ["/r/demo/~tours"] },
          React.createElement(SearchSpy),
          React.createElement(
            Routes,
            null,
            React.createElement(Route, {
              path: "/r/:repo/~tours",
              element: React.createElement(Tours),
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

/// A select's change is not a click: set the value through the NATIVE setter
/// (React's own value tracker would swallow a plain assignment) and dispatch
/// the change a real pick produces, OUTSIDE act() — the same
/// gesture-and-read a spec performs.
function pickOption(sel: HTMLSelectElement, value: string) {
  const setter = Object.getOwnPropertyDescriptor(HTMLSelectElement.prototype, "value")!.set!;
  setter.call(sel, value);
  sel.dispatchEvent(new Event("change", { bubbles: true }));
}

describe("Tours status-select (react-router 7 startTransition, V76-R4d.2)", () => {
  beforeEach(() => {
    lastSearch = "";
    globalThis.fetch = (async () =>
      new Response(JSON.stringify(TOURS_LIST), {
        status: 200,
        headers: { "Content-Type": "application/json" },
      })) as typeof fetch;
  });

  afterEach(() => {
    document.body.innerHTML = "";
  });

  it("select flips SYNCHRONOUSLY on change (no waitFor window)", async () => {
    const { container } = renderTours();
    // The status options come from the list query — let it land first.
    await settle();
    const sel = container.querySelector<HTMLSelectElement>("[data-kbc-tours-status]");
    expect(sel).toBeTruthy();
    expect(sel!.value).toBe("");
    // Pre-fix this assertion fails: the select stays at "" until the
    // router's transition commits.
    pickOption(sel!, "draft");
    expect(sel!.value).toBe("draft");
  });

  it("optimistic state reconciles with the URL once the navigation lands", async () => {
    const { container } = renderTours();
    await settle();
    const sel = container.querySelector<HTMLSelectElement>("[data-kbc-tours-status]")!;
    pickOption(sel, "draft");
    expect(sel.value).toBe("draft");
    await settle();
    expect(lastSearch).toContain("status=draft");
    // After reconciliation the select still shows the pick — the effect
    // that re-reads the URL must not fight the optimistic value.
    expect(sel.value).toBe("draft");
  });
});
