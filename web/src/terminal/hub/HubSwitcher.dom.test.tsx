// @vitest-environment jsdom
//
// HUBSWITCH — the switcher column through the real component with mocked data hooks.
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { act, cleanup, fireEvent, render, screen, within } from "@testing-library/react";
import { MemoryRouter, Route, Routes, useLocation } from "react-router-dom";
import type { TermRow } from "../../api/terms";
import type { LiveStatusRow } from "../../api/generated/LiveStatusRow";

const m = vi.hoisted(() => ({
  terms: { terms: [] as unknown[], isLoading: false, isError: false },
  live: { rows: [] as unknown[], loading: false },
}));
vi.mock("../useTerms", () => ({ useTerms: () => m.terms }));
vi.mock("../../hooks/useLiveSessions", () => ({ useLiveSessions: () => m.live }));
vi.mock("../../hooks/useNowTick", () => ({ useNowTick: () => 1_000_000_000_000 }));

import { HubSwitcher } from "./HubSwitcher";
import { __resetMarks, setMarks } from "../notify/attention";
import { fixtureTerms, mkLive, mkTerm } from "./switchTestKit";

function Where() {
  const l = useLocation();
  return <div data-testid="where">{l.pathname + l.search}</div>;
}

function mount(currentId: string | null) {
  return render(
    <MemoryRouter initialEntries={[currentId ? `/terminals/${currentId}` : "/terminals"]}>
      <Routes>
        <Route
          path="*"
          element={
            <>
              <HubSwitcher currentId={currentId} />
              <Where />
            </>
          }
        />
      </Routes>
    </MemoryRouter>,
  );
}

const rowIds = (root: HTMLElement): string[] =>
  [...root.querySelectorAll("[data-switch-row]")].map((e) => e.getAttribute("data-term") ?? "");

beforeEach(() => {
  m.terms = { terms: fixtureTerms(), isLoading: false, isError: false };
  m.live = { rows: [], loading: false };
  window.localStorage.clear();
});
afterEach(() => {
  cleanup();
  act(() => __resetMarks());
});

describe("HubSwitcher", () => {
  it("groups This project / Everywhere, needs first, ended collapsed", () => {
    mount("b");
    const this_ = screen.getByTestId("switch-this");
    const every = screen.getByTestId("switch-everywhere");
    expect(within(this_).getByRole("heading").textContent).toBe("This project");
    expect(within(every).getByRole("heading").textContent).toBe("Everywhere");
    expect(rowIds(this_)).toEqual(["a", "b"]);
    expect(rowIds(every)).toEqual(["e", "c"]);
    const ended = screen.getByTestId("switch-ended") as HTMLDetailsElement;
    expect(ended.open).toBe(false);
    expect(within(ended).getByText("Ended (1)")).toBeTruthy();
    expect(rowIds(ended)).toEqual(["d"]);
    expect(screen.getByTestId("switch-needs-count").textContent).toBe("2 need you");
  });

  it("marks the open row aria-current=page and no other", () => {
    mount("b");
    const rows = screen.getAllByTestId("switch-row");
    expect(rows.filter((r) => r.getAttribute("aria-current") === "page").map((r) => r.getAttribute("data-term"))).toEqual([
      "b",
    ]);
  });

  it("on the hub page (no open terminal) there is no This project group", () => {
    mount(null);
    expect(screen.queryByTestId("switch-this")).toBeNull();
    expect(rowIds(screen.getByTestId("switch-everywhere"))).toEqual(["e", "a", "b", "c"]);
  });

  it("renders title, project, harness, last line and the tier chip from the join", () => {
    m.terms = { terms: [mkTerm("x", { cwd: "/w/p9", harness: "omp", state: "working" })], isLoading: false, isError: false };
    m.live = { rows: [mkLive("s-x", { title: "Fix the thing", last_line: "running tests", state: "working" })], loading: false };
    mount("x");
    const row = screen.getByTestId("switch-row");
    expect(row.textContent).toContain("Fix the thing");
    expect(row.textContent).toContain("p9 · omp");
    expect(row.textContent).toContain("running tests");
    expect(row.textContent).toContain("working");
  });

  it("asks and finished marks carry different text and test ids", () => {
    act(() =>
      setMarks({
        c: { asks: true, finishedUnseen: false },
        b: { asks: false, finishedUnseen: true },
      }),
    );
    mount("a");
    const c = document.querySelector('[data-switch-row][data-term="c"]') as HTMLElement;
    const b = document.querySelector('[data-switch-row][data-term="b"]') as HTMLElement;
    expect(within(c).getByTestId("mark-asks").textContent).toContain("asks");
    expect(within(c).queryByTestId("mark-finished")).toBeNull();
    expect(within(b).getByTestId("mark-finished").textContent).toContain("finished");
    expect(within(b).queryByTestId("mark-asks")).toBeNull();
    expect(within(c).getByTestId("mark-asks").textContent).not.toBe(within(b).getByTestId("mark-finished").textContent);
  });

  it("a mark written after mount appears without a reload", () => {
    mount("a");
    expect(screen.queryByTestId("mark-asks")).toBeNull();
    act(() => setMarks({ c: { asks: true, finishedUnseen: false } }));
    expect(screen.getByTestId("mark-asks")).toBeTruthy();
  });

  it("a row is a link to its terminal", () => {
    mount("a");
    const link = document.querySelector('[data-switch-row][data-term="c"]') as HTMLAnchorElement;
    expect(link.getAttribute("href")).toBe("/terminals/c");
    fireEvent.click(link);
    expect(screen.getByTestId("where").textContent).toBe("/terminals/c");
  });

  it("empty state when there are no terminals", () => {
    m.terms = { terms: [] as TermRow[], isLoading: false, isError: false };
    m.live = { rows: [] as LiveStatusRow[], loading: false };
    mount(null);
    expect(screen.getByTestId("switch-empty")).toBeTruthy();
    expect(screen.getByTestId("switch-needs-count").textContent).toBe("0 need you");
  });

  it("Up/Down move focus between rows (roving) and skip the collapsed Ended group", () => {
    mount("b");
    const get = (id: string) => document.querySelector(`[data-switch-row][data-term="${id}"]`) as HTMLElement;
    // roving tabindex: the open row is the one tab stop
    expect(get("b").tabIndex).toBe(0);
    expect(get("a").tabIndex).toBe(-1);
    act(() => get("b").focus());
    fireEvent.keyDown(get("b"), { key: "ArrowDown" });
    expect(document.activeElement).toBe(get("e"));
    fireEvent.keyDown(get("e"), { key: "ArrowDown" });
    expect(document.activeElement).toBe(get("c"));
    fireEvent.keyDown(get("c"), { key: "ArrowDown" }); // d is inside the closed Ended group
    expect(document.activeElement).toBe(get("c"));
    fireEvent.keyDown(get("c"), { key: "ArrowUp" });
    expect(document.activeElement).toBe(get("e"));
    expect(get("e").tabIndex).toBe(0);
    expect(get("b").tabIndex).toBe(-1);
  });
});
