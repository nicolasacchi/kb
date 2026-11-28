// @vitest-environment jsdom
//
// HUBSWITCH — the frame: desktop column + persisted toggle, phone = children only,
// and the shell child is not remounted when the terminal id changes.
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { cleanup, fireEvent, render, screen } from "@testing-library/react";
import { MemoryRouter } from "react-router-dom";

vi.mock("../useTerms", () => ({ useTerms: () => ({ terms: [], isLoading: false, isError: false }) }));
vi.mock("../../hooks/useLiveSessions", () => ({ useLiveSessions: () => ({ rows: [], loading: false }) }));
vi.mock("../../hooks/useNowTick", () => ({ useNowTick: () => 1_000_000_000_000 }));

import { HubFrame } from "./HubFrame";
import { SWITCH_OPEN_KEY } from "./switchModel";

function stubMedia(matches: boolean): void {
  vi.stubGlobal(
    "matchMedia",
    (q: string) =>
      ({ matches, media: q, addEventListener() {}, removeEventListener() {}, addListener() {}, removeListener() {} }) as unknown as MediaQueryList,
  );
}

function Child({ tag }: { tag: string }) {
  return <div data-testid="child">{tag}</div>;
}

const frame = (id: string, child: React.ReactNode) => (
  <MemoryRouter>
    <HubFrame termId={id}>{child}</HubFrame>
  </MemoryRouter>
);

beforeEach(() => {
  window.localStorage.clear();
  stubMedia(false);
});
afterEach(() => {
  cleanup();
  vi.unstubAllGlobals();
  vi.restoreAllMocks();
});

describe("HubFrame", () => {
  it("desktop: the column is shown, open by default", () => {
    render(frame("t1", <Child tag="x" />));
    expect(screen.getByTestId("hub-switcher")).toBeTruthy();
    const t = screen.getByTestId("hub-switch-toggle");
    expect(t.getAttribute("aria-expanded")).toBe("true");
    expect(document.getElementById(t.getAttribute("aria-controls") ?? "")).toBeTruthy();
  });

  it("the toggle collapses the column and persists to kb-term-switch-open", () => {
    const { unmount } = render(frame("t1", <Child tag="x" />));
    fireEvent.click(screen.getByTestId("hub-switch-toggle"));
    expect(window.localStorage.getItem(SWITCH_OPEN_KEY)).toBe("0");
    expect(screen.queryByTestId("hub-switcher")).toBeNull();
    expect(screen.getByTestId("hub-switch-toggle").getAttribute("aria-expanded")).toBe("false");
    unmount();
    render(frame("t1", <Child tag="x" />));
    expect(screen.queryByTestId("hub-switcher")).toBeNull();
    fireEvent.click(screen.getByTestId("hub-switch-toggle"));
    expect(window.localStorage.getItem(SWITCH_OPEN_KEY)).toBe("1");
    expect(screen.getByTestId("hub-switcher")).toBeTruthy();
  });

  it("a collapsed column mounts no key listener", () => {
    window.localStorage.setItem(SWITCH_OPEN_KEY, "0");
    const add = vi.spyOn(document, "addEventListener");
    render(frame("t1", <Child tag="x" />));
    expect(add.mock.calls.filter((c) => c[0] === "keydown")).toEqual([]);
  });

  it("phone: children only, no column, no toggle, no key listener", () => {
    stubMedia(true);
    const add = vi.spyOn(document, "addEventListener");
    render(frame("t1", <Child tag="x" />));
    expect(screen.getByTestId("child")).toBeTruthy();
    expect(screen.queryByTestId("hub-frame")).toBeNull();
    expect(screen.queryByTestId("hub-switcher")).toBeNull();
    expect(screen.queryByTestId("hub-switch-toggle")).toBeNull();
    expect(add.mock.calls.filter((c) => c[0] === "keydown")).toEqual([]);
  });

  it("the child element survives a termId change (no remount)", () => {
    const { rerender } = render(frame("t1", <Child tag="x" />));
    const el = screen.getByTestId("child");
    const col = screen.getByTestId("hub-switcher");
    rerender(frame("t2", <Child tag="x" />));
    expect(screen.getByTestId("child")).toBe(el);
    expect(screen.getByTestId("hub-switcher")).toBe(col);
  });
});
