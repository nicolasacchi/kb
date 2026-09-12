// @vitest-environment jsdom
// V76-R4d round 3b — pin for review-diff-v2.spec.ts:235: the patchset
// switcher's `?ps=1` write was LOST, overwritten by the hunk cursor's
// `?hunk=` echo (CI saw `…/~reviews/13/diff?hunk=c9135785898e750e`).
//
// Mechanism: `useReviewDiffState`'s writers used to build the next query
// string from the render-time `searchParams` snapshot, and under react-router
// 7 the navigation commit is deferred (`React.startTransition`) — so a second
// write issued before the first commits rebuilds the OLD params and the
// first write vanishes. The fix routes every writer through
// `lib/codeUrl.ts`'s `mergeCurrentSearch`, which merges onto
// `window.location.search` AT CALL TIME (the router's history layer updates
// it synchronously when `navigate` runs, committed or not).
//
// The gesture below is exactly the CI sequence, compressed: `ps=1` and the
// `?hunk=` echo land BACK TO BACK in the same tick — before any transition
// can commit — and the URL must end with BOTH keys. Verified failing pre-fix
// (the URL held only `hunk=…`).
import { describe, expect, it } from "vitest";
import * as React from "react";
import { createRoot, type Root } from "react-dom/client";
import { act } from "react-dom/test-utils";
import { BrowserRouter } from "react-router";
import { useReviewDiffState } from "./useReviewDiffState";

(globalThis as { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = true;

describe("useReviewDiffState — back-to-back writes never clobber (V76-R4d.3)", () => {
  it("ps=1 then the hunk echo in the same tick: BOTH keys survive", () => {
    window.history.replaceState(null, "", "/r/demo/~reviews/13/diff");
    let api: ReturnType<typeof useReviewDiffState> | null = null;
    function Probe() {
      api = useReviewDiffState();
      return null;
    }
    const container = document.createElement("div");
    document.body.appendChild(container);
    const root: Root = createRoot(container);
    act(() => {
      root.render(React.createElement(BrowserRouter, null, React.createElement(Probe)));
    });
    expect(api).not.toBeNull();

    // The selectOption("1") write and the cursor→?hunk= echo, issued before
    // the router's deferred commit for the first write can land — the exact
    // race CI hit. No act() between them on purpose.
    act(() => {
      api!.setPs(1);
      api!.setParam("hunk", "c9135785898e750e");
    });

    expect(window.location.search).toContain("ps=1");
    expect(window.location.search).toContain("hunk=c9135785898e750e");

    act(() => root.unmount());
    container.remove();
  });

  it("the hunk echo merges its own key in — it never drops keys it does not own", () => {
    window.history.replaceState(null, "", "/r/demo/~reviews/13/diff?ps=1&noise=collapsed");
    let api: ReturnType<typeof useReviewDiffState> | null = null;
    function Probe() {
      api = useReviewDiffState();
      return null;
    }
    const container = document.createElement("div");
    document.body.appendChild(container);
    const root: Root = createRoot(container);
    act(() => {
      root.render(React.createElement(BrowserRouter, null, React.createElement(Probe)));
    });

    act(() => {
      api!.setParam("hunk", "deadbeefdeadbeef");
    });

    expect(window.location.search).toContain("hunk=deadbeefdeadbeef");
    expect(window.location.search).toContain("ps=1");
    expect(window.location.search).toContain("noise=collapsed");

    act(() => root.unmount());
    container.remove();
  });

  it("setExpanded keeps a same-tick ps write (the collapseOnTick writers)", () => {
    window.history.replaceState(null, "", "/r/demo/~reviews/13/diff");
    let api: ReturnType<typeof useReviewDiffState> | null = null;
    function Probe() {
      api = useReviewDiffState();
      return null;
    }
    const container = document.createElement("div");
    document.body.appendChild(container);
    const root: Root = createRoot(container);
    act(() => {
      root.render(React.createElement(BrowserRouter, null, React.createElement(Probe)));
    });

    act(() => {
      api!.setPs(1);
      api!.setExpanded(["a.rb"], []);
    });

    expect(window.location.search).toContain("ps=1");
    expect(window.location.search).toContain("expanded=a.rb");

    act(() => root.unmount());
    container.remove();
  });
});
