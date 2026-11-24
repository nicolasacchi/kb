// @vitest-environment jsdom
//
// BSHELL — the shell's layout contract: slots, the desktop rail (roving tabs,
// collapse, persistence) and the phone's non-modal bottom layer (collapsed by
// default, content mounted only while expanded).
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { act, cleanup, fireEvent, render, screen } from "@testing-library/react";
import { TermShell, RAIL_OPEN_KEY, RAIL_TAB_KEY } from "./TermShell";
import { registerRailTab, resetRailTabs } from "./rail/registry";

function media(matches: boolean) {
  vi.stubGlobal(
    "matchMedia",
    (q: string) =>
      ({ matches, media: q, addEventListener() {}, removeEventListener() {}, addListener() {}, removeListener() {} }) as unknown as MediaQueryList,
  );
}

beforeEach(() => {
  window.localStorage.clear();
  media(false);
});
afterEach(() => {
  cleanup();
  resetRailTabs();
  vi.unstubAllGlobals();
});

const shell = (extra: Partial<React.ComponentProps<typeof TermShell>> = {}) => (
  <TermShell termId="t1" harness="claude" {...extra}>
    <div data-testid="pane">pane</div>
  </TermShell>
);

describe("TermShell slots", () => {
  it("carries the workspace label and renders header, pane, rail in order", () => {
    render(shell({ header: <div data-testid="hdr">h</div> }));
    const root = screen.getByTestId("term-shell");
    expect(root.getAttribute("aria-label")).toBe("Terminal workspace");
    expect(root.getAttribute("data-kb-term-layout")).toBe("desktop");
    const order = ["hdr", "pane"].map((id) => screen.getByTestId(id));
    expect(order[0].compareDocumentPosition(order[1]) & Node.DOCUMENT_POSITION_FOLLOWING).toBeTruthy();
    const rail = root.querySelector("aside") as HTMLElement;
    expect(order[1].compareDocumentPosition(rail) & Node.DOCUMENT_POSITION_FOLLOWING).toBeTruthy();
  });

  it("statusChips render as a list; none renders nothing", () => {
    const { unmount } = render(shell({ statusChips: [{ id: "m", label: "opus", tone: "warn" }] }));
    expect(screen.getByRole("list").textContent).toContain("opus");
    unmount();
    render(shell());
    expect(screen.queryByRole("list")).toBeNull();
  });
});

describe("TermShell desktop rail", () => {
  it("has three tabs with the placeholder panel and roving arrow keys", () => {
    render(shell());
    const tabs = screen.getAllByRole("tab");
    expect(tabs.map((t) => t.textContent)).toEqual(["Conversation", "Files", "Commands"]);
    expect(screen.getByRole("tablist")).toBeTruthy();
    expect(screen.getByTestId("rail-placeholder-conversation").textContent).toBe("Coming soon");
    expect(tabs.map((t) => t.getAttribute("tabindex"))).toEqual(["0", "-1", "-1"]);
    fireEvent.keyDown(tabs[0], { key: "ArrowRight" });
    expect(screen.getByTestId("rail-placeholder-files")).toBeTruthy();
    expect(screen.getAllByRole("tab")[1].getAttribute("aria-selected")).toBe("true");
    expect(document.activeElement).toBe(screen.getAllByRole("tab")[1]);
    fireEvent.keyDown(screen.getAllByRole("tab")[1], { key: "End" });
    expect(screen.getByTestId("rail-placeholder-commands")).toBeTruthy();
    fireEvent.keyDown(screen.getAllByRole("tab")[2], { key: "ArrowRight" });
    expect(screen.getByTestId("rail-placeholder-conversation")).toBeTruthy();
    fireEvent.keyDown(screen.getAllByRole("tab")[0], { key: "ArrowLeft" });
    expect(screen.getByTestId("rail-placeholder-commands")).toBeTruthy();
    fireEvent.keyDown(screen.getAllByRole("tab")[2], { key: "Home" });
    expect(screen.getByTestId("rail-placeholder-conversation")).toBeTruthy();
  });

  it("a registered tab replaces its placeholder", () => {
    registerRailTab({ id: "files", label: "Files", badge: 2, node: <div data-testid="real-files" /> });
    render(shell());
    fireEvent.click(screen.getByTestId("term-tab-files"));
    expect(screen.getByTestId("real-files")).toBeTruthy();
    expect(screen.getByTestId("term-tab-files").textContent).toContain("2");
  });

  it("the toggle collapses the rail, hides the panel and persists across a remount", () => {
    const { unmount } = render(shell());
    const toggle = screen.getByTestId("term-rail-toggle");
    expect(toggle.getAttribute("aria-expanded")).toBe("true");
    fireEvent.click(toggle);
    expect(toggle.getAttribute("aria-expanded")).toBe("false");
    expect(screen.queryByRole("tabpanel")).toBeNull();
    expect(window.localStorage.getItem(RAIL_OPEN_KEY)).toBe("0");
    unmount();
    render(shell());
    expect(screen.getByTestId("term-rail-toggle").getAttribute("aria-expanded")).toBe("false");
    expect(screen.queryByRole("tabpanel")).toBeNull();
  });

  it("persists the open tab, and corrupt stored values fall back", () => {
    const { unmount } = render(shell());
    fireEvent.click(screen.getByTestId("term-tab-commands"));
    expect(window.localStorage.getItem(RAIL_TAB_KEY)).toBe("commands");
    unmount();
    render(shell());
    expect(screen.getByTestId("rail-placeholder-commands")).toBeTruthy();
    cleanup();
    window.localStorage.setItem(RAIL_TAB_KEY, "bogus");
    window.localStorage.setItem(RAIL_OPEN_KEY, "maybe");
    render(shell());
    expect(screen.getByTestId("rail-placeholder-conversation")).toBeTruthy();
    expect(screen.getByTestId("term-rail-toggle").getAttribute("aria-expanded")).toBe("true");
  });
});

describe("TermShell phone layer", () => {
  beforeEach(() => media(true));

  it("renders the pane unchanged, marks the layout phone, and starts collapsed", () => {
    render(shell());
    expect(screen.getByTestId("term-shell").getAttribute("data-kb-term-layout")).toBe("phone");
    expect(screen.getByTestId("pane")).toBeTruthy();
    expect(screen.queryByTestId("term-rail-toggle")).toBeNull();
    expect(screen.getByTestId("term-sheet-handle").getAttribute("aria-expanded")).toBe("false");
    expect(screen.queryByRole("tablist")).toBeNull();
    expect(screen.queryByTestId("rail-placeholder-conversation")).toBeNull();
  });

  it("expanding mounts the tab content; collapsing unmounts it again", () => {
    render(shell());
    const handle = screen.getByTestId("term-sheet-handle");
    fireEvent.click(handle);
    expect(handle.getAttribute("aria-expanded")).toBe("true");
    expect(screen.getAllByRole("tab")).toHaveLength(3);
    expect(screen.getByTestId("rail-placeholder-conversation")).toBeTruthy();
    fireEvent.click(handle);
    expect(handle.getAttribute("aria-expanded")).toBe("false");
    expect(screen.queryByTestId("rail-placeholder-conversation")).toBeNull();
  });

  it("Esc collapses only when focus is inside the layer", () => {
    render(shell());
    fireEvent.click(screen.getByTestId("term-sheet-handle"));
    const handle = screen.getByTestId("term-sheet-handle");
    handle.focus();
    fireEvent.keyDown(handle, { key: "Escape" });
    expect(handle.getAttribute("aria-expanded")).toBe("false");
    fireEvent.click(handle);
    (document.activeElement as HTMLElement).blur();
    fireEvent.keyDown(screen.getByTestId("term-sheet"), { key: "Escape" });
    expect(screen.getByTestId("term-sheet-handle").getAttribute("aria-expanded")).toBe("true");
  });
});

describe("TermShell layout switch", () => {
  it("crossing the phone breakpoint keeps the pane mounted (same node, no remount)", () => {
    let narrow = false;
    const listeners: Array<() => void> = [];
    vi.stubGlobal(
      "matchMedia",
      (q: string) =>
        ({
          get matches() {
            return narrow;
          },
          media: q,
          addEventListener: (_: string, l: () => void) => listeners.push(l),
          removeEventListener() {},
          addListener() {},
          removeListener() {},
        }) as unknown as MediaQueryList,
    );
    render(shell());
    const pane = screen.getByTestId("pane");
    expect(screen.getByTestId("term-shell").getAttribute("data-kb-term-layout")).toBe("desktop");
    narrow = true;
    act(() => listeners.forEach((l) => l()));
    expect(screen.getByTestId("term-shell").getAttribute("data-kb-term-layout")).toBe("phone");
    expect(screen.getByTestId("pane")).toBe(pane);
    expect(pane.isConnected).toBe(true);
  });
});
