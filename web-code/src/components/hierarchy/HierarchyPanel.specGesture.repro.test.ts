// @vitest-environment jsdom
// SCRATCH repro (V76-R4d round 3b) — hierarchy.spec.ts:22-51's EXACT gesture,
// through the real CodeView/vim keymap, the real cursorUrlSync debounce, the
// real HierarchyPanel + hierarchyReducer, and Reader.tsx's handleHierClose
// idiom. Keys are dispatched at document.activeElement (what Playwright's
// keyboard.press does), never at the panel directly.
import { describe, expect, it } from "vitest";
import * as React from "react";
import { createRoot, type Root } from "react-dom/client";
import { act } from "react-dom/test-utils";
import { flushSync } from "react-dom";
import { BrowserRouter, useNavigate } from "react-router";
import CodeView, { type CodeViewHandle } from "../CodeView";
import HierarchyPanel from "./HierarchyPanel";
import { createCursorUrlSync, type CursorUrlSync } from "../../lib/cursorUrlSync";
import {
  buildCallersTree,
  hierarchyReducer,
  initialHierarchyState,
  updateNode,
  type HierarchyNode,
} from "../../lib/hierarchyState";
import type { HierarchyCallersOut } from "../../api/types";

(globalThis as { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = true;

// jsdom lacks scrollIntoView — stub it so the panel's scroll effect runs.
if (!Element.prototype.scrollIntoView) Element.prototype.scrollIntoView = () => undefined;

const CALLERS: HierarchyCallersOut = {
  function: { name: "target_fn", path: "src/lib.rs", line: 10, col: 0, kind: "function" },
  callers: [
    {
      path: "src/caller.rs",
      enclosing: { name: "helper", line: 3, kind: "function" },
      sites: [{ line: 5, col: 2, class: "exact" }],
    },
  ],
  truncated: false,
} as unknown as HierarchyCallersOut;

const CONTENT = Array.from({ length: 40 }, (_, i) => `line ${i + 1} target_fn`).join("\n");

const focusLog: string[] = [];
function tag(el: Element | null): string {
  if (!el) return "null";
  const he = el as HTMLElement;
  return `${el.tagName}.${he.className?.toString().slice(0, 40)}${he.dataset?.kbcHierarchy ? "[HIER]" : ""}`;
}

function Harness() {
  const navigate = useNavigate();
  const [hier, dispatchHier] = React.useReducer(hierarchyReducer, initialHierarchyState);
  const viewRef = React.useRef<CodeViewHandle | null>(null);
  const syncRef = React.useRef<CursorUrlSync | null>(null);

  React.useEffect(() => {
    const sync = createCursorUrlSync({
      replace: (search) => navigate({ search }, { replace: true }),
      getSearch: () => window.location.search,
    });
    syncRef.current = sync;
    return () => sync.dispose();
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  async function onHierarchyCallers() {
    await new Promise((r) => setTimeout(r, 0)); // resolve hop
    dispatchHier({ type: "OPEN", mode: "callers", title: "target_fn" });
    await new Promise((r) => setTimeout(r, 0)); // fetch hop
    dispatchHier({ type: "SET_TREE", roots: buildCallersTree(CALLERS) });
  }

  async function onToggleExpand(node: HierarchyNode) {
    if (node.expanded || node.children.length > 0 || node.depth === 0) {
      dispatchHier({
        type: "PATCH_ROOTS",
        roots: updateNode(hier.roots, node.id, (n) => ({ ...n, expanded: true })),
      });
      return;
    }
    dispatchHier({
      type: "PATCH_ROOTS",
      roots: updateNode(hier.roots, node.id, (n) => ({ ...n, loading: true })),
    });
    await new Promise((r) => setTimeout(r, 0)); // fetch hop
    dispatchHier({
      type: "PATCH_ROOTS",
      roots: updateNode(hier.roots, node.id, (n) => ({ ...n, loading: false, expanded: true })),
    });
  }

  function handleHierClose() {
    flushSync(() => dispatchHier({ type: "CLOSE" }));
    viewRef.current?.focus();
  }

  return React.createElement(
    React.Fragment,
    null,
    React.createElement(CodeView, {
      ref: viewRef,
      content: CONTENT,
      spans: null,
      blobHash: "b1",
      vim: { onHierarchyCallers: () => void onHierarchyCallers() },
      onSelectionLines: (sel: { start: number; end: number }) => syncRef.current?.onSelection(sel),
    }),
    hier.open &&
      React.createElement(HierarchyPanel, {
        state: hier,
        currentRepo: "demo",
        anchor: null,
        onMove: (delta: number) => dispatchHier({ type: "MOVE", delta }),
        onActivate: () => undefined,
        onToggleExpand: (node: HierarchyNode) => void onToggleExpand(node),
        onClose: handleHierClose,
      }),
  );
}

function key(target: Element, k: string) {
  target.dispatchEvent(new KeyboardEvent("keydown", { key: k, bubbles: true, cancelable: true }));
}

const tick = () => new Promise((r) => setTimeout(r, 0));

describe("hierarchy close — the spec's gesture (round 3b repro)", () => {
  it("gc, j, l, Escape — panel unmounts synchronously at Escape", async () => {
    const container = document.createElement("div");
    document.body.appendChild(container);
    const root: Root = createRoot(container);
    await act(async () => {
      root.render(React.createElement(BrowserRouter, null, React.createElement(Harness)));
      await tick();
    });

    const cm = container.querySelector(".cm-content") as HTMLElement;
    expect(cm).toBeTruthy();
    // The spec's click lands focus in the buffer and moves the selection
    // (the debounced cursor→?line= sync now has a pending write).
    await act(async () => {
      cm.focus();
      cm.dispatchEvent(new MouseEvent("mousedown", { bubbles: true }));
      await tick();
    });
    // Move the cursor so onSelectionLines fires (the click's equivalent).
    key(cm, "j");
    await act(async () => {
      await tick();
    });

    // gc — opens the hierarchy panel (async, two hops).
    key(cm, "g");
    key(cm, "c");
    let panel: Element | null = null;
    for (let i = 0; i < 20 && !panel; i++) {
      await act(async () => {
        await tick();
      });
      panel = container.querySelector("[data-kbc-hierarchy]");
    }
    expect(panel).toBeTruthy();
    focusLog.push(`after open: ${tag(document.activeElement)}`);

    // Spec: j, l (panel keys).
    key(document.activeElement as Element, "j");
    key(document.activeElement as Element, "l");
    await act(async () => {
      await tick();
    });
    focusLog.push(`after j/l: ${tag(document.activeElement)}`);

    // Let the debounced cursor→?line= navigate fire (transition pending).
    await act(async () => {
      await new Promise((r) => setTimeout(r, 600));
    });
    focusLog.push(`after debounce: ${tag(document.activeElement)} search=${window.location.search}`);

    // The spec's close gesture: Escape at the focused element, then an
    // immediate toHaveCount(0) — no waitFor.
    key(document.activeElement as Element, "Escape");
    focusLog.push(`at Escape target was: ${tag(document.activeElement)}`);
    console.log("FOCUS TRACE:\n" + focusLog.join("\n"));
    expect(container.querySelector("[data-kbc-hierarchy]")).toBeNull();
  });
});
