// @vitest-environment jsdom
import { useState } from "react";
import { afterEach, describe, expect, it, vi } from "vitest";
import { cleanup, render, screen } from "@testing-library/react";
import ErrorBoundary from "./ErrorBoundary";

// invariant:32 — `resetKey` (a plain prop, cleared via componentDidUpdate)
// must clear a caught error WITHOUT force-remounting a healthy child the
// way a React `key` would. A `useState` lazy initializer assigns each
// PROBE INSTANCE a stable id exactly once at mount — reused across
// re-renders of the same fiber, bumped only by a genuine remount — so it
// is the cheapest possible "did this actually remount?" probe. Migrated
// off react-test-renderer (deprecated upstream as of React 19 — see
// react.dev's v19 upgrade guide) onto @testing-library/react: the probe
// already rendered a plain DOM node, so this is a query-by-DOM swap with
// no loss of assertion power.
let nextId = 0;
function Probe({ shouldThrow }: { shouldThrow: boolean }) {
  const [id] = useState(() => ++nextId);
  if (shouldThrow) throw new Error("boom");
  return (
    <div data-testid="probe" data-id={id}>
      ok
    </div>
  );
}

function probeId(): number {
  return Number(screen.getByTestId("probe").getAttribute("data-id"));
}

describe("ErrorBoundary resetKey-not-key (invariant #32)", () => {
  const errSpy = vi.spyOn(console, "error").mockImplementation(() => {});
  afterEach(() => {
    cleanup();
    errSpy.mockClear();
  });

  it("a resetKey change alone does NOT remount a healthy child", () => {
    const { rerender } = render(
      <ErrorBoundary resetKey="/a">
        <Probe shouldThrow={false} />
      </ErrorBoundary>,
    );
    const firstId = probeId();

    // Simulate an in-app route change: same tree shape, new resetKey.
    rerender(
      <ErrorBoundary resetKey="/b">
        <Probe shouldThrow={false} />
      </ErrorBoundary>,
    );
    expect(probeId()).toBe(firstId); // same fiber, not remounted
  });

  it("a React `key` on the same wrapper WOULD force a remount (the regression this guards)", () => {
    const { rerender } = render(
      <ErrorBoundary key="/a">
        <Probe shouldThrow={false} />
      </ErrorBoundary>,
    );
    const firstId = probeId();

    rerender(
      <ErrorBoundary key="/b">
        <Probe shouldThrow={false} />
      </ErrorBoundary>,
    );
    expect(probeId()).not.toBe(firstId); // new fiber: remounted
  });

  it("a resetKey change DOES clear a caught error and re-renders the children", () => {
    const { rerender } = render(
      <ErrorBoundary resetKey="/broken">
        <Probe shouldThrow={true} />
      </ErrorBoundary>,
    );
    expect(screen.getByText("Something went wrong")).toBeTruthy();

    // Route change to a healthy artifact: new resetKey, child no longer throws.
    rerender(
      <ErrorBoundary resetKey="/healthy">
        <Probe shouldThrow={false} />
      </ErrorBoundary>,
    );
    const node = screen.getByTestId("probe");
    expect(node.textContent).toBe("ok");
  });
});
