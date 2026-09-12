// @vitest-environment jsdom
// V76-R4d round 2 — pin for the hierarchy panel close (hierarchy.spec.ts:51).
//
// The panel's open/closed state is LOCAL (`hierarchyReducer` in
// `lib/hierarchyState.ts`), but under react-router 7 every navigation is a
// React.startTransition, and a local dispatch made while such a transition
// is pending is entangled with it — the close commit is deferred until the
// transition settles, so Esc left `[data-kbc-hierarchy]` mounted past the
// spec's assertion window.
//
// `Reader.tsx`'s `handleHierClose` now wraps the CLOSE dispatch in
// `flushSync`, forcing the commit on the same tick as the keydown. This
// test mounts the REAL `HierarchyPanel` + the REAL reducer behind the same
// handler idiom, starts a pending navigation transition, and asserts the
// panel is gone IMMEDIATELY after the Escape keydown — no `waitFor`, which
// would hide the defect (the deferred close does commit eventually).
//
// jsdom is opted into per-file (the suite default is `node`, deliberately):
// this defect only exists in the DOM commit path.
import { describe, expect, it } from "vitest";
import * as React from "react";
import { createRoot, type Root } from "react-dom/client";
import { act } from "react-dom/test-utils";
import { flushSync } from "react-dom";
import { MemoryRouter, useSearchParams } from "react-router";
import HierarchyPanel from "./HierarchyPanel";
import {
  hierarchyReducer,
  initialHierarchyState,
  type HierarchyAction,
  type HierarchyNode,
} from "../../lib/hierarchyState";

(globalThis as { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = true;

/// Mirrors `Reader.tsx`'s `handleHierClose` (V76-R4d round 2): the CLOSE
/// dispatch is flushSync-wrapped so Esc unmounts the panel on the same
/// tick even while a router transition is pending.
function closeHierarchyNow(dispatch: (a: HierarchyAction) => void): void {
  flushSync(() => dispatch({ type: "CLOSE" }));
}

function Harness() {
  const [hier, dispatch] = React.useReducer(hierarchyReducer, initialHierarchyState);
  const [, setSearchParams] = useSearchParams();
  React.useEffect(() => {
    dispatch({ type: "OPEN", mode: "callers", title: "target" });
  }, []);
  return React.createElement(
    React.Fragment,
    null,
    // Starts a router navigation WITHOUT flushing — leaving a transition
    // pending, the exact condition the Reader is in when its debounced
    // cursor→URL sync fires around a keypress.
    React.createElement(
      "button",
      {
        "data-nav": true,
        onClick: () => setSearchParams({ line: "7" }, { replace: true }),
      },
      "nav",
    ),
    React.createElement(HierarchyPanel, {
      state: hier,
      currentRepo: "demo",
      anchor: null,
      onMove: (delta: number) => dispatch({ type: "MOVE", delta }),
      onActivate: (_node: HierarchyNode) => undefined,
      onToggleExpand: (_node: HierarchyNode) => undefined,
      onClose: () => closeHierarchyNow(dispatch),
    }),
  );
}

describe("HierarchyPanel close (react-router 7 startTransition, V76-R4d round 2)", () => {
  it("Escape unmounts [data-kbc-hierarchy] SYNCHRONOUSLY despite a pending navigation", () => {
    const container = document.createElement("div");
    document.body.appendChild(container);
    const root: Root = createRoot(container);
    act(() => {
      root.render(React.createElement(MemoryRouter, null, React.createElement(Harness)));
    });
    const panel = container.querySelector("[data-kbc-hierarchy]");
    expect(panel).toBeTruthy();

    // Begin a transition and do NOT let it settle.
    (container.querySelector("[data-nav]") as HTMLButtonElement).click();
    // The spec's gesture: Esc to the focused panel, then an immediate
    // toHaveCount(0) — read the DOM right after the event, no waiting.
    panel!.dispatchEvent(new KeyboardEvent("keydown", { key: "Escape", bubbles: true }));
    expect(container.querySelector("[data-kbc-hierarchy]")).toBeNull();
  });
});
