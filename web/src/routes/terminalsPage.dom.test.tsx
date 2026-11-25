// @vitest-environment jsdom
//
// HUBDOOR — /terminals, the hub list page, through the real page with mocked
// data hooks.
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { cleanup, fireEvent, render, screen, within } from "@testing-library/react";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { MemoryRouter, Route, Routes, useLocation } from "react-router-dom";
import type { TermRow } from "../api/terms";
import type { LiveStatusRow } from "../api/generated/LiveStatusRow";

const m = vi.hoisted(() => ({
  identity: { terminal_admitted: true } as { terminal_admitted: boolean } | null,
  terms: { terms: [] as unknown[], isLoading: false, isError: false },
  live: { rows: [] as unknown[], loading: false },
}));
vi.mock("../hooks/useArtifactHost", () => ({ useIdentity: () => m.identity }));
vi.mock("../terminal/useTerms", () => ({ useTerms: () => m.terms }));
vi.mock("../hooks/useLiveSessions", () => ({ useLiveSessions: () => m.live }));
vi.mock("../hooks/useNowTick", () => ({ useNowTick: () => 1_000_000_000_000 }));

import TerminalsPageRoute from "./terminalsPage";

const term = (id: string, o: Partial<TermRow> = {}): TermRow => ({
  id,
  harness: "claude",
  cwd: "/w/alpha",
  started_unix: 100,
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
  ...o,
});
const liveRow = (sid: string, o: Partial<LiveStatusRow> = {}): LiveStatusRow =>
  ({
    session_id: sid,
    harness: "claude",
    holder: "agent",
    state: "working",
    source: "hook",
    confidence: "observed",
    since_unix: 500,
    since_secs: 1,
    resume: "",
    blocked: false,
    ...o,
  }) as unknown as LiveStatusRow;

function Where() {
  const l = useLocation();
  return <div data-testid="where">{l.pathname + l.search}</div>;
}

function mount(url = "/terminals") {
  const qc = new QueryClient();
  const spy = vi.spyOn(qc, "invalidateQueries");
  render(
    <QueryClientProvider client={qc}>
      <MemoryRouter initialEntries={[url]}>
        <Routes>
          <Route path="/terminals" element={<><TerminalsPageRoute /><Where /></>} />
          <Route path="/terminals/:id" element={<Where />} />
        </Routes>
      </MemoryRouter>
    </QueryClientProvider>,
  );
  return { spy };
}

const ids = () => screen.queryAllByTestId("hub-row").map((r) => r.getAttribute("data-term"));

beforeEach(() => {
  m.identity = { terminal_admitted: true };
  m.terms = { terms: [], isLoading: false, isError: false };
  m.live = { rows: [], loading: false };
});
afterEach(cleanup);

describe("/terminals", () => {
  it("identity loading -> loading card; not admitted -> refused card", () => {
    m.identity = null;
    mount();
    expect(screen.getByText("Loading terminals")).toBeTruthy();
    cleanup();
    m.identity = { terminal_admitted: false };
    mount();
    expect(screen.getByText("Terminals are not enabled")).toBeTruthy();
    expect(screen.queryByTestId("hub-page")).toBeNull();
  });

  it("terms loading -> loading card", () => {
    m.terms = { terms: [], isLoading: true, isError: false };
    mount();
    expect(screen.getByText("Loading terminals")).toBeTruthy();
  });

  it("error with no rows -> degraded card whose Retry invalidates [terms]", () => {
    m.terms = { terms: [], isLoading: false, isError: true };
    const { spy } = mount();
    const box = screen.getByTestId("hub-list-error");
    fireEvent.click(within(box).getByText("Retry"));
    expect(spy).toHaveBeenCalledWith({ queryKey: ["terms"] });
  });

  it("zero terms -> empty card with no launch button", () => {
    mount();
    const box = screen.getByTestId("hub-empty");
    expect(box.querySelector("button")).toBeNull();
  });

  it("orders needs-you (oldest block first), working, idle, then ended inside the collapsed group", () => {
    m.terms = {
      isLoading: false,
      isError: false,
      terms: [
        term("idle1", { started_unix: 50 }),
        term("dead", { pane_dead: true, started_unix: 400 }),
        term("work", { state: "working", started_unix: 60 }),
        term("blkNew", { blocked: true, started_unix: 70 }),
        term("blkOld", { blocked: true, started_unix: 10 }),
      ],
    };
    mount();
    expect(ids()).toEqual(["blkOld", "blkNew", "work", "idle1", "dead"]);
    const live = screen.getByTestId("hub-live");
    expect(within(live).queryByText("ended")).toBeNull();
    const ended = screen.getByTestId("hub-ended") as HTMLDetailsElement;
    expect(ended.open).toBe(false);
    expect(within(ended).getAllByTestId("hub-row").map((r) => r.getAttribute("data-term"))).toEqual(["dead"]);
    expect(screen.getByTestId("hub-needs-count").textContent).toBe("2 need you");
    expect(screen.getAllByTestId("hub-row")[0].getAttribute("data-tier")).toBe("needs");
  });

  it("row links to /terminals/<id>", () => {
    m.terms = { isLoading: false, isError: false, terms: [term("t1")] };
    mount();
    const row = screen.getByTestId("hub-row");
    expect(row.getAttribute("href")).toBe("/terminals/t1");
    fireEvent.click(row);
    expect(screen.getByTestId("where").textContent).toBe("/terminals/t1");
  });

  it("?needs=1 and ?project= filter the list; filtered to nothing -> empty card with Clear filters", () => {
    m.terms = {
      isLoading: false,
      isError: false,
      terms: [
        term("a", { blocked: true, cwd: "/w/alpha" }),
        term("b", { cwd: "/w/beta" }),
      ],
    };
    mount("/terminals?needs=1");
    expect(ids()).toEqual(["a"]);
    cleanup();
    mount("/terminals?project=beta");
    expect(ids()).toEqual(["b"]);
    cleanup();
    mount("/terminals?project=beta&needs=1");
    expect(ids()).toEqual([]);
    fireEvent.click(within(screen.getByTestId("hub-no-match")).getByText("Clear filters"));
    expect(screen.getByTestId("where").textContent).toBe("/terminals");
    expect(ids()).toEqual(["a", "b"]);
  });

  it("filter chips are links that carry the filter in the URL", () => {
    m.terms = { isLoading: false, isError: false, terms: [term("a", { cwd: "/w/alpha" })] };
    mount();
    const chips = screen.getAllByTestId("hub-chip");
    const hrefs = chips.map((c) => c.getAttribute("href"));
    expect(hrefs).toContain("/terminals?needs=1");
    expect(hrefs).toContain("/terminals?project=alpha");
    expect(hrefs).toContain("/terminals?harness=claude");
  });

  it("live-status loading or empty never blocks the list; a bound live row adds its title and last line", () => {
    m.terms = {
      isLoading: false,
      isError: false,
      terms: [term("a", { session_id: "s1" }), term("b")],
    };
    m.live = { rows: [], loading: true };
    mount();
    expect(ids()).toEqual(["a", "b"]);
    cleanup();
    m.live = { rows: [liveRow("s1", { title: "Fix the thing", last_line: "running tests" })], loading: false };
    mount();
    const row = screen.getAllByTestId("hub-row").find((r) => r.getAttribute("data-term") === "a")!;
    expect(row.textContent).toContain("Fix the thing");
    expect(row.textContent).toContain("running tests");
  });

  it("a live-blocked row counts as needs-you even when the term row is not blocked", () => {
    m.terms = { isLoading: false, isError: false, terms: [term("a", { session_id: "s1" }), term("b")] };
    m.live = { rows: [liveRow("s1", { blocked: true, blocked_since_unix: 5 })], loading: false };
    mount();
    expect(ids()).toEqual(["a", "b"]);
    expect(screen.getByTestId("hub-needs-count").textContent).toBe("1 need you");
  });
});
