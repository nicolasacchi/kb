// @vitest-environment jsdom
//
// BSHELL — the shell's layout contract: slots, the desktop rail (roving tabs,
// collapse, persistence) and the phone's non-modal bottom layer (collapsed by
// default, content mounted only while expanded).
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { act, cleanup, fireEvent, render, screen } from "@testing-library/react";
import { TermShell, RAIL_OPEN_KEY, RAIL_TAB_KEY } from "./TermShell";
const closeReq = vi.hoisted(() => vi.fn());
vi.mock("@kb/ui/useCloseRequest", () => ({ useCloseRequest: closeReq }));
// The real Conversation tab needs a query client; these tests pin the shell
// itself against the placeholders (the tab has its own tests).
vi.mock("./rail/registerTabs", () => ({}));
// CONVNEED: the banner has its own tests; here only the shell wiring is pinned.
vi.mock("./needs/NeedsYouBanner", () => ({
  NeedsYouBanner: ({ onJump }: { onJump: () => void }) => (
    <div data-testid="needs-you">
      <button type="button" data-testid="needs-you-jump" onClick={onJump} />
    </div>
  ),
}));
import { hasPendingJump, resetPendingJumpForTests } from "./needs/pendingJump";
import { registerRailTab, resetRailTabs } from "./rail/registry";
import { useTermRail } from "./rail/context";

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
  resetPendingJumpForTests();
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
  it("has seven tabs with the placeholder panel and roving arrow keys", () => {
    render(shell());
    const tabs = screen.getAllByRole("tab");
    expect(tabs.map((t) => t.textContent)).toEqual([
      "Now",
      "Conversation",
      "Changes",
      "Files",
      "Knowledge",
      "Board",
      "Commands",
    ]);
    expect(screen.getByRole("tablist")).toBeTruthy();
    expect(screen.getByTestId("rail-placeholder-conversation").textContent).toBe("Coming soon");
    expect(tabs.map((t) => t.getAttribute("tabindex"))).toEqual(["-1", "0", "-1", "-1", "-1", "-1", "-1"]);
    fireEvent.keyDown(tabs[1], { key: "ArrowRight" });
    expect(screen.getByTestId("rail-placeholder-changes")).toBeTruthy();
    expect(screen.getAllByRole("tab")[2].getAttribute("aria-selected")).toBe("true");
    expect(document.activeElement).toBe(screen.getAllByRole("tab")[2]);
    fireEvent.keyDown(screen.getAllByRole("tab")[2], { key: "End" });
    expect(screen.getByTestId("rail-placeholder-commands")).toBeTruthy();
    fireEvent.keyDown(screen.getAllByRole("tab")[6], { key: "ArrowRight" });
    expect(screen.getByTestId("rail-placeholder-now")).toBeTruthy();
    fireEvent.keyDown(screen.getAllByRole("tab")[0], { key: "ArrowLeft" });
    expect(screen.getByTestId("rail-placeholder-commands")).toBeTruthy();
    fireEvent.keyDown(screen.getAllByRole("tab")[6], { key: "Home" });
    expect(screen.getByTestId("rail-placeholder-now")).toBeTruthy();
  });

  it("the rail context carries the terminal id, harness and bound session id", () => {
    function Probe() {
      const c = useTermRail();
      return <div data-testid="ctx">{`${c.termId}|${c.harness}|${c.sid ?? "-"}`}</div>;
    }
    registerRailTab({ id: "conversation", label: "Conversation", node: <Probe /> });
    render(shell({ sid: "sid-9" }));
    expect(screen.getByTestId("ctx").textContent).toBe("t1|claude|sid-9");
    cleanup();
    render(shell());
    expect(screen.getByTestId("ctx").textContent).toBe("t1|claude|-");
  });

  it("a registered tab replaces its placeholder", () => {
    registerRailTab({ id: "files", label: "Files", badge: 2, node: <div data-testid="real-files" /> });
    render(shell());
    fireEvent.click(screen.getByTestId("term-tab-files"));
    expect(screen.getByTestId("real-files")).toBeTruthy();
    expect(screen.getByTestId("term-tab-files").textContent).toContain("2");
  });

  it("a useBadge hook badges its tab live and only above zero", () => {
    let n: number | undefined = 0;
    registerRailTab({ id: "board", label: "Board", useBadge: () => n, node: <div /> });
    const { rerender } = render(shell());
    expect(screen.getByTestId("term-tab-board").querySelector(".kb-termshell__badge")).toBeNull();
    n = 3;
    rerender(shell());
    expect(screen.getByTestId("term-tab-board").querySelector(".kb-termshell__badge")?.textContent).toBe("3");
    n = undefined;
    rerender(shell());
    expect(screen.getByTestId("term-tab-board").querySelector(".kb-termshell__badge")).toBeNull();
    expect(screen.getByTestId("term-tab-files").querySelector(".kb-termshell__badge")).toBeNull();
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

  it("registers a close request only while expanded, and invoking it collapses", () => {
    render(shell());
    const last = () => closeReq.mock.calls[closeReq.mock.calls.length - 1];
    expect(last()[0]).toBe(false);
    fireEvent.click(screen.getByTestId("term-sheet-handle"));
    expect(last()[0]).toBe(true);
    act(() => last()[1]());
    expect(screen.getByTestId("term-sheet-handle").getAttribute("aria-expanded")).toBe("false");
    expect(last()[0]).toBe(false);
  });

  it("expanding mounts the tab content; collapsing unmounts it again", () => {
    render(shell());
    const handle = screen.getByTestId("term-sheet-handle");
    fireEvent.click(handle);
    expect(handle.getAttribute("aria-expanded")).toBe("true");
    expect(screen.getAllByRole("tab")).toHaveLength(7);
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

describe("TermShell needs-you banner", () => {
  const follows = (a: HTMLElement, b: HTMLElement) =>
    !!(a.compareDocumentPosition(b) & Node.DOCUMENT_POSITION_FOLLOWING);

  it("sits between the bar and the body, outside the rail", () => {
    render(shell({ header: <div data-testid="hdr">h</div> }));
    const root = screen.getByTestId("term-shell");
    const banner = screen.getByTestId("needs-you");
    expect(banner.parentElement).toBe(screen.getByTestId("term-needs-slot"));
    expect(screen.getByTestId("term-needs-slot").parentElement).toBe(root);
    expect(follows(screen.getByTestId("hdr"), banner)).toBe(true);
    expect(follows(banner, screen.getByTestId("pane"))).toBe(true);
  });

  it("is still present on the phone with the sheet collapsed, and not inside the sheet", () => {
    media(true);
    render(shell());
    expect(screen.getByTestId("term-sheet-handle").getAttribute("aria-expanded")).toBe("false");
    const banner = screen.getByTestId("needs-you");
    expect(screen.getByTestId("term-sheet").contains(banner)).toBe(false);
  });

  it("jump (desktop) selects the conversation tab, reopens a collapsed rail and requests the row focus", () => {
    window.localStorage.setItem(RAIL_TAB_KEY, "commands");
    window.localStorage.setItem(RAIL_OPEN_KEY, "0");
    render(shell());
    expect(screen.getByTestId("term-rail-toggle").getAttribute("aria-expanded")).toBe("false");
    expect(hasPendingJump()).toBe(false);
    fireEvent.click(screen.getByTestId("needs-you-jump"));
    expect(window.localStorage.getItem(RAIL_TAB_KEY)).toBe("conversation");
    expect(window.localStorage.getItem(RAIL_OPEN_KEY)).toBe("1");
    expect(screen.getByTestId("term-rail-toggle").getAttribute("aria-expanded")).toBe("true");
    expect(screen.getByTestId("rail-placeholder-conversation")).toBeTruthy();
    expect(hasPendingJump()).toBe(true);
  });

  it("jump (phone) expands the sheet on the conversation tab", () => {
    media(true);
    window.localStorage.setItem(RAIL_TAB_KEY, "commands");
    render(shell());
    fireEvent.click(screen.getByTestId("needs-you-jump"));
    expect(screen.getByTestId("term-sheet-handle").getAttribute("aria-expanded")).toBe("true");
    expect(window.localStorage.getItem(RAIL_TAB_KEY)).toBe("conversation");
    expect(screen.getByTestId("rail-placeholder-conversation")).toBeTruthy();
    expect(hasPendingJump()).toBe(true);
  });
});

describe("BA11Y: rail tabs follow the WAI-ARIA tabs pattern", () => {
  const stops = (): string[] => screen.getAllByRole("tab").map((t) => t.getAttribute("tabindex") ?? "");
  const active = (): number => screen.getAllByRole("tab").findIndex((t) => t.getAttribute("aria-selected") === "true");

  it("key table: Left/Right wrap, Home/End jump, Up/Down and modified arrows are ignored, one tab stop always", () => {
    render(shell());
    const tab = (i: number): HTMLElement => screen.getAllByRole("tab")[i];
    expect(active()).toBe(1);
    fireEvent.keyDown(tab(1), { key: "ArrowDown" });
    fireEvent.keyDown(tab(1), { key: "ArrowUp" });
    fireEvent.keyDown(tab(1), { key: "ArrowRight", ctrlKey: true });
    fireEvent.keyDown(tab(1), { key: "ArrowRight", shiftKey: true });
    expect(active()).toBe(1);
    fireEvent.keyDown(tab(1), { key: "Home" });
    expect(active()).toBe(0);
    expect(stops().filter((s) => s === "0")).toHaveLength(1);
    fireEvent.keyDown(tab(0), { key: "ArrowLeft" });
    expect(active()).toBe(6);
    fireEvent.keyDown(tab(6), { key: "ArrowRight" });
    expect(active()).toBe(0);
    fireEvent.keyDown(tab(0), { key: "End" });
    expect(active()).toBe(6);
    expect(stops().filter((s) => s === "0")).toHaveLength(1);
    expect(stops()[6]).toBe("0");
  });

  it("keys from a non-tab target in the tablist are not taken", () => {
    render(shell());
    fireEvent.keyDown(screen.getByRole("tablist"), { key: "End" });
    expect(active()).toBe(1);
  });

  it("wires orientation, aria-controls and a labelled focusable tabpanel", () => {
    render(shell());
    expect(screen.getByRole("tablist").getAttribute("aria-orientation")).toBe("horizontal");
    const panel = screen.getByRole("tabpanel");
    const on = screen.getAllByRole("tab")[active()];
    expect(on.getAttribute("aria-controls")).toBe(panel.id);
    expect(panel.getAttribute("aria-labelledby")).toBe(on.id);
    expect(panel.getAttribute("tabindex")).toBe("0");
  });
});
