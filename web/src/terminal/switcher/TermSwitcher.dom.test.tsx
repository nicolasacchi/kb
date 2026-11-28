// @vitest-environment jsdom
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { act, cleanup, fireEvent, render, screen } from "@testing-library/react";
import { MemoryRouter, Route, Routes, useLocation } from "react-router-dom";
import type { TermRow } from "../../api/terms";
import { __resetMarks, setMarks } from "../notify/attention";

const m = vi.hoisted(() => ({
  admitted: true as boolean | null,
  terms: [] as unknown[],
  isLoading: false,
  isError: false,
}));
vi.mock("../../hooks/useArtifactHost", () => ({
  useIdentity: () => (m.admitted === null ? null : { terminal_admitted: m.admitted }),
}));
vi.mock("../useTerms", () => ({
  useTerms: () => ({ terms: m.terms, isLoading: m.isLoading, isError: m.isError }),
}));
vi.mock("../../hooks/useLiveSessions", () => ({
  useLiveSessions: () => ({ rows: [], loading: false }),
}));

import TermSwitcher from "./TermSwitcher";

function term(id: string, o: Partial<TermRow> = {}): TermRow {
  return {
    id,
    harness: "claude",
    cwd: `/work/${id}`,
    started_unix: 1000,
    resume_sid: null,
    pane_pid: 1,
    cols: 80,
    rows: 24,
    bracket_paste_flag: false,
    pane_dead: false,
    exit_code: null,
    grid_owner: null,
    conf_version: 1,
    anchor_pid: null,
    stale: false,
    session_id: null,
    state: null,
    blocked: false,
    ...o,
  };
}

function Where() {
  return <div data-testid="where">{useLocation().pathname}</div>;
}

function mount(currentId = "a") {
  return render(
    <MemoryRouter initialEntries={[`/terminals/${currentId}`]}>
      <TermSwitcher currentId={currentId} />
      <textarea data-testid="pane" />
      <Routes>
        <Route path="*" element={<Where />} />
      </Routes>
    </MemoryRouter>,
  );
}

const where = () => screen.getByTestId("where").textContent;
const btn = () => screen.getByTestId("kb-term-switcher-btn");

function chord(code: string, o: KeyboardEventInit = {}, target: Element = document.body) {
  return fireEvent.keyDown(target, { key: code, code, altKey: true, ...o });
}

beforeEach(() => {
  m.admitted = true;
  m.isLoading = false;
  m.isError = false;
  m.terms = [
    term("a", { cwd: "/work/p", started_unix: 10 }),
    term("b", { cwd: "/work/p", started_unix: 20 }),
    term("c", { cwd: "/work/q", started_unix: 30 }),
    term("x", { cwd: "/work/p", pane_dead: true }),
  ];
});
afterEach(() => {
  cleanup();
  __resetMarks();
});

describe("TermSwitcher button", () => {
  it("renders nothing unless terminal-admitted", () => {
    m.admitted = false;
    const { container } = mount();
    expect(container.querySelector('[data-testid="kb-term-switcher"]')).toBeNull();
  });

  it("shows the live count (ended excluded) and no badge when nothing needs you", () => {
    mount();
    expect(screen.getByTestId("kb-term-switcher-count").textContent).toBe("3");
    expect(screen.queryByTestId("kb-term-switcher-badge")).toBeNull();
    expect(btn().getAttribute("aria-label")).toBe("Terminals, 3 running");
    expect(btn().getAttribute("aria-haspopup")).toBe("listbox");
  });

  it("a blocked sibling shows badge 1 with a matching aria-label", () => {
    m.terms = [...m.terms, term("z", { blocked: true, cwd: "/work/q" })];
    mount();
    expect(screen.getByTestId("kb-term-switcher-badge").textContent).toBe("1");
    expect(btn().getAttribute("aria-label")).toBe("Terminals, 4 running, 1 needs you");
  });

  it("an asks mark from the attention store raises the badge", () => {
    mount();
    act(() => setMarks({ c: { asks: true, finishedUnseen: false } }));
    expect(screen.getByTestId("kb-term-switcher-badge").textContent).toBe("1");
  });
});

describe("TermSwitcher popover", () => {
  it("opens a listbox in place (not portalled) with groups, roles and mark text", () => {
    setMarks({
      c: { asks: true, finishedUnseen: false },
      b: { asks: false, finishedUnseen: true },
    });
    mount();
    fireEvent.click(btn());
    const pop = screen.getByTestId("kb-term-switcher-pop");
    expect(pop.closest('[data-testid="kb-term-switcher"]')).not.toBeNull();
    expect(btn().getAttribute("aria-expanded")).toBe("true");
    const list = screen.getByRole("listbox");
    const groups = screen.getAllByRole("group");
    expect(groups.map((g) => g.textContent?.startsWith("This project"))).toEqual([true, false]);
    expect(screen.getAllByRole("option").map((o) => o.getAttribute("data-term"))).toEqual(["b", "a", "c"]);
    expect(list.getAttribute("aria-activedescendant")).toBeTruthy();
    const rowC = screen.getAllByRole("option").find((o) => o.getAttribute("data-term") === "c")!;
    const rowB = screen.getAllByRole("option").find((o) => o.getAttribute("data-term") === "b")!;
    expect(rowC.textContent).toContain("needs you");
    expect(rowB.textContent).toContain("finished");
    expect(rowC.getAttribute("data-mark")).toBe("asks");
    expect(rowB.getAttribute("data-mark")).toBe("finished");
    expect(screen.getAllByRole("option").find((o) => o.getAttribute("data-term") === "a")!.getAttribute("aria-current")).toBe("true");
  });

  it("lists This project first, needs-you first inside each group", () => {
    setMarks({ a: { asks: true, finishedUnseen: false } });
    mount("b");
    fireEvent.click(btn());
    const ids = screen.getAllByRole("option").map((o) => o.getAttribute("data-term"));
    expect(ids).toEqual(["a", "b", "c"]);
  });

  it("clicking a row navigates to /terminals/<id> and closes", () => {
    mount();
    fireEvent.click(btn());
    const rowC = screen.getAllByRole("option").find((o) => o.getAttribute("data-term") === "c")!;
    fireEvent.click(rowC);
    expect(where()).toBe("/terminals/c");
    expect(screen.queryByTestId("kb-term-switcher-pop")).toBeNull();
  });

  it("arrow keys move the active descendant and Enter navigates", () => {
    mount();
    fireEvent.click(btn());
    const list = screen.getByRole("listbox");
    const first = list.getAttribute("aria-activedescendant");
    fireEvent.keyDown(list, { key: "ArrowDown" });
    const second = list.getAttribute("aria-activedescendant");
    expect(second).not.toBe(first);
    fireEvent.keyDown(list, { key: "ArrowDown" });
    fireEvent.keyDown(list, { key: "Enter" });
    expect(where()).toBe("/terminals/c");
  });

  it("Escape closes and returns focus to the button", () => {
    mount();
    fireEvent.click(btn());
    fireEvent.keyDown(screen.getByRole("listbox"), { key: "Escape" });
    expect(screen.queryByTestId("kb-term-switcher-pop")).toBeNull();
    expect(document.activeElement).toBe(btn());
  });

  it("an outside pointerdown closes it", () => {
    mount();
    fireEvent.click(btn());
    fireEvent.pointerDown(screen.getByTestId("pane"));
    expect(screen.queryByTestId("kb-term-switcher-pop")).toBeNull();
  });

  it("shows loading, error and empty captions", () => {
    m.terms = [];
    m.isLoading = true;
    const a = mount();
    fireEvent.click(btn());
    expect(screen.getByText("Loading terminals…")).toBeTruthy();
    a.unmount();
    m.isLoading = false;
    m.isError = true;
    const b = mount();
    fireEvent.click(btn());
    expect(screen.getByText("Cannot read the terminal list.")).toBeTruthy();
    b.unmount();
    m.isError = false;
    mount();
    fireEvent.click(btn());
    expect(screen.getByText("No running terminals.")).toBeTruthy();
  });

  it("with only the current terminal it says there is no other", () => {
    m.terms = [term("a")];
    mount();
    fireEvent.click(btn());
    expect(screen.getByText("No other terminals.")).toBeTruthy();
  });
});

describe("TermSwitcher Alt chords", () => {
  it("Alt+ArrowDown / Alt+ArrowUp step through the flat list, wrapping", () => {
    mount("a"); // flat: c, b, a
    chord("ArrowDown");
    expect(where()).toBe("/terminals/c");
  });

  it("Alt+ArrowUp steps back", () => {
    mount("a");
    chord("ArrowUp");
    expect(where()).toBe("/terminals/b");
  });

  it("Alt+N goes to the next needs-you terminal", () => {
    setMarks({ c: { asks: true, finishedUnseen: false } });
    mount("a");
    chord("KeyN");
    expect(where()).toBe("/terminals/c");
  });

  it("plain, Ctrl+Alt and Alt+Shift keys do not navigate", () => {
    mount("a");
    fireEvent.keyDown(document.body, { key: "ArrowDown", code: "ArrowDown" });
    chord("ArrowDown", { ctrlKey: true });
    chord("ArrowDown", { shiftKey: true });
    chord("KeyN", { metaKey: true });
    expect(where()).toBe("/terminals/a");
  });

  it("a matched chord never reaches a pane listener; an unmatched key does", () => {
    mount("a");
    const pane = screen.getByTestId("pane");
    const seen: string[] = [];
    pane.addEventListener("keydown", (e) => seen.push((e as KeyboardEvent).code));
    const notPrevented = chord("ArrowDown", {}, pane);
    expect(notPrevented).toBe(false); // preventDefault was called
    fireEvent.keyDown(pane, { key: "n", code: "KeyN" });
    expect(seen).toEqual(["KeyN"]);
  });

  it("removes its window listener on unmount", () => {
    const add = vi.spyOn(window, "addEventListener");
    const rm = vi.spyOn(window, "removeEventListener");
    const r = mount("a");
    expect(add.mock.calls.some((c) => c[0] === "keydown" && c[2] === true)).toBe(true);
    r.unmount();
    expect(rm.mock.calls.some((c) => c[0] === "keydown" && c[2] === true)).toBe(true);
    const pane = document.createElement("textarea");
    document.body.appendChild(pane);
    const seen: string[] = [];
    pane.addEventListener("keydown", (e) => seen.push((e as KeyboardEvent).code));
    chord("ArrowDown", {}, pane);
    expect(seen).toEqual(["ArrowDown"]);
    pane.remove();
    add.mockRestore();
    rm.mockRestore();
  });

  it("registers no listener when the caller is not admitted", () => {
    m.admitted = false;
    const add = vi.spyOn(window, "addEventListener");
    mount("a");
    expect(add.mock.calls.some((c) => c[0] === "keydown")).toBe(false);
    add.mockRestore();
  });
});
