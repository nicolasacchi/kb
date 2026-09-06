import { act, create } from "react-test-renderer";
import { createElement, useState } from "react";
import { afterEach, describe, expect, it, vi } from "vitest";
import ErrorBoundary from "./ErrorBoundary";

// invariant:32 — `resetKey` (a plain prop, cleared via componentDidUpdate)
// must clear a caught error WITHOUT force-remounting a healthy child the
// way a React `key` would. A `useState` lazy initializer assigns each
// PROBE INSTANCE a stable id exactly once at mount — reused across
// re-renders of the same fiber, bumped only by a genuine remount — so it
// is the cheapest possible "did this actually remount?" probe (no DOM,
// no lifecycle-timing subtlety).
let nextId = 0;
function Probe({ shouldThrow }: { shouldThrow: boolean }) {
  const [id] = useState(() => ++nextId);
  if (shouldThrow) throw new Error("boom");
  return createElement("div", { "data-testid": "probe", "data-id": id }, "ok");
}

function probeId(root: ReturnType<typeof create>["root"]): number {
  return Number(root.findByProps({ "data-testid": "probe" }).props["data-id"]);
}

describe("ErrorBoundary resetKey-not-key (invariant #32)", () => {
  const errSpy = vi.spyOn(console, "error").mockImplementation(() => {});
  afterEach(() => errSpy.mockClear());

  it("a resetKey change alone does NOT remount a healthy child", () => {
    let renderer!: ReturnType<typeof create>;
    act(() => {
      renderer = create(
        createElement(ErrorBoundary, {
          resetKey: "/a",
          children: createElement(Probe, { shouldThrow: false }),
        }),
      );
    });
    const firstId = probeId(renderer.root);

    // Simulate an in-app route change: same tree shape, new resetKey.
    act(() => {
      renderer.update(
        createElement(ErrorBoundary, {
          resetKey: "/b",
          children: createElement(Probe, { shouldThrow: false }),
        }),
      );
    });
    expect(probeId(renderer.root)).toBe(firstId); // same fiber, not remounted
  });

  it("a React `key` on the same wrapper WOULD force a remount (the regression this guards)", () => {
    let renderer!: ReturnType<typeof create>;
    act(() => {
      renderer = create(
        createElement(ErrorBoundary, {
          key: "/a",
          children: createElement(Probe, { shouldThrow: false }),
        }),
      );
    });
    const firstId = probeId(renderer.root);

    act(() => {
      renderer.update(
        createElement(ErrorBoundary, {
          key: "/b",
          children: createElement(Probe, { shouldThrow: false }),
        }),
      );
    });
    expect(probeId(renderer.root)).not.toBe(firstId); // new fiber: remounted
  });

  it("a resetKey change DOES clear a caught error and re-renders the children", () => {
    let renderer!: ReturnType<typeof create>;
    act(() => {
      renderer = create(
        createElement(ErrorBoundary, {
          resetKey: "/broken",
          children: createElement(Probe, { shouldThrow: true }),
        }),
      );
    });
    expect(JSON.stringify(renderer.toJSON())).toContain(
      "Something went wrong",
    );

    // Route change to a healthy artifact: new resetKey, child no longer throws.
    act(() => {
      renderer.update(
        createElement(ErrorBoundary, {
          resetKey: "/healthy",
          children: createElement(Probe, { shouldThrow: false }),
        }),
      );
    });
    const node = renderer.root.findByProps({ "data-testid": "probe" });
    expect(node.props.children).toBe("ok");
  });
});
