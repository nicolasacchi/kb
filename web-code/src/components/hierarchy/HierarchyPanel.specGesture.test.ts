// @vitest-environment jsdom
// V76-R4d.4 — the PIN for hierarchy.spec.ts:22-51's gesture: `gc` on a call
// site, panel keys, then the close. Runs through the real CodeView/vim
// keymap, the real cursorUrlSync debounce, the real HierarchyPanel +
// hierarchyReducer, Reader.tsx's handleHierClose idiom — AND the real
// CommandRoot with a registered `reader.compare` handler, which is what the
// round-3b repro lacked: the double-fire is CommandRoot ALSO firing
// `reader.compare` on the `c` that the vim layer already consumed as the
// `gc` continuation (vim preventDefaults but never stopPropagations). Keys
// are dispatched at document.activeElement (what Playwright's keyboard.press
// does), never at the panel directly.
//
// Pre-fix this pin FAILS on the `compareCalls` assertion below (the `c` of
// `gc` fired reader.compare once — the double-fire's signature). Post-fix,
// CommandRoot honours `e.defaultPrevented` and stands down.
import { describe, expect, it } from "vitest";
import * as React from "react";
import { createRoot, type Root } from "react-dom/client";
import { act } from "react-dom/test-utils";
import { flushSync } from "react-dom";
import { BrowserRouter, useNavigate } from "react-router";
import CodeView, { type CodeViewHandle } from "../CodeView";
import HierarchyPanel from "./HierarchyPanel";
import CommandRoot, { useCommandHandlers, useCommandScope } from "../../commands/CommandRoot";
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

interface HarnessProps {
  /// Counts every central dispatch of `reader.compare` — the double-fire's
  /// signature. The vim layer consumes the `c` of `gc`; if CommandRoot ALSO
  /// fires, this increments.
  compareCalls: { n: number };
}

function Harness({ compareCalls }: HarnessProps) {
  const navigate = useNavigate();
  const [hier, dispatchHier] = React.useReducer(hierarchyReducer, initialHierarchyState);
  const viewRef = React.useRef<CodeViewHandle | null>(null);
  const syncRef = React.useRef<CursorUrlSync | null>(null);

  // The Reader route's own wiring: reader scope while the buffer is focused
  // (it is, for this whole gesture), and a central `reader.compare` handler —
  // Reader.tsx registers one for the bare `c` row.
  useCommandScope("reader", {});
  useCommandHandlers({
    "reader.compare": () => {
      compareCalls.n += 1;
    },
  });

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

describe("hierarchy close — the spec's gesture (V76-R4d.4 pin)", () => {
  it("gc, j, l, Escape — panel closes, and the c of gc never fires reader.compare", async () => {
    const compareCalls = { n: 0 };
    const container = document.createElement("div");
    document.body.appendChild(container);
    const root: Root = createRoot(container);
    await act(async () => {
      root.render(
        React.createElement(
          CommandRoot,
          null,
          React.createElement(BrowserRouter, null, React.createElement(Harness, { compareCalls })),
        ),
      );
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

    // gc — the vim layer consumes BOTH keys (`g` prefixes, `c` completes
    // cb-hierarchy-callers) and opens the hierarchy panel (async, two hops).
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

    // THE PIN: the `c` of `gc` was already consumed by the vim layer, so
    // CommandRoot must NOT have fired `reader.compare` on it too. Pre-fix
    // this is 1 — the double-fire that re-fires/re-mounts the panel and
    // keeps hierarchy.spec.ts:51's toHaveCount(0) from ever reaching 0.
    expect(compareCalls.n).toBe(0);

    // Spec: j, l (panel keys).
    key(document.activeElement as Element, "j");
    key(document.activeElement as Element, "l");
    await act(async () => {
      await tick();
    });

    // Let the debounced cursor→?line= navigate fire (transition pending).
    await act(async () => {
      await new Promise((r) => setTimeout(r, 600));
    });

    // The spec's close gesture: Escape at the focused element, then an
    // immediate toHaveCount(0) — no waitFor.
    key(document.activeElement as Element, "Escape");
    expect(container.querySelector("[data-kbc-hierarchy]")).toBeNull();
    expect(compareCalls.n).toBe(0);

    // And a REAL bare `c` still works, exactly once: focus is back in the
    // buffer (handleHierClose's viewRef focus), bare `c` is inert in the
    // read-only vim layer (vimKeys.test.ts), so CommandRoot owns it.
    key(document.activeElement as Element, "c");
    expect(compareCalls.n).toBe(1);
  });
});
