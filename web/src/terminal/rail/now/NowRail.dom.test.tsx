// @vitest-environment jsdom
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { cleanup, fireEvent, render, renderHook, screen, within } from "@testing-library/react";
import { MemoryRouter, useLocation } from "react-router-dom";
import type { ReactNode } from "react";

const live = vi.hoisted(() => ({ rows: [] as unknown[] }));
const cardMock = vi.hoisted(() => ({ card: null as unknown }));
const convMock = vi.hoisted(() => ({ data: undefined as unknown, calls: [] as unknown[] }));
const detailMock = vi.hoisted(() => ({ detail: null as unknown }));
const boardMock = vi.hoisted(() => ({ board: null as unknown }));
const termsMock = vi.hoisted(() => ({ terms: [] as unknown[], isError: false }));

vi.mock("../../../hooks/useLiveSessions", () => ({ useLiveSessions: () => ({ rows: live.rows, loading: false }) }));
vi.mock("../../../hooks/useSessionCard", () => ({ useSessionCard: () => ({ card: cardMock.card, loading: false, error: null }) }));
vi.mock("../../../hooks/useConversation", () => ({
  useConversation: (sid: string | undefined) => {
    convMock.calls.push(sid);
    return { data: convMock.data };
  },
}));
vi.mock("../../../hooks/useSessions", () => ({ useSessionDetail: () => ({ detail: detailMock.detail, loading: false }) }));
vi.mock("../../../hooks/useSlates", () => ({ useSlateBoard: () => ({ board: boardMock.board, loading: false, error: null }) }));
vi.mock("../../useTerms", () => ({ useTerms: () => ({ terms: termsMock.terms, isLoading: false, isError: termsMock.isError }) }));
vi.mock("../../../hooks/useNowTick", () => ({ useNowTick: () => 1_700_000_000_000 }));

import { NowRail } from "./NowRail";
import { useNowBadge } from "./useNowBadge";
import { TermRailContext } from "../context";
import { __resetMarks, setMarks } from "../../notify/attention";
import { board, card as slateCard } from "../board/boardRows.testkit";

const NOW = 1_700_000_000;
const SID = "sid-1";

function Loc() {
  return <span data-testid="loc">{useLocation().pathname}</span>;
}

function wrap(sid: string | undefined) {
  return ({ children }: { children: ReactNode }) => (
    <MemoryRouter>
      <TermRailContext.Provider value={{ termId: "t1", harness: "claude", sid }}>
        {children}
        <Loc />
      </TermRailContext.Provider>
    </MemoryRouter>
  );
}

const liveRow = (over: Record<string, unknown> = {}) => ({
  session_id: SID,
  harness: "claude",
  state: "working",
  blocked: false,
  since_unix: NOW - 10,
  ...over,
});

const term = (id: string, over: Record<string, unknown> = {}) => ({ id, harness: "claude", cwd: "/work/proj", pane_dead: false, started_unix: NOW - 100, ...over });

const fullCard = (over: Record<string, unknown> = {}) => ({
  outcome: { available: true, last_assistant_text: "Final answer text" },
  children: { available: true, total: 2 },
  stats: {
    available: true,
    model: "claude-opus-4-8",
    stats: {
      input_tokens: 10, output_tokens: 20, cache_read_tokens: 30, cache_write_tokens: 40, cache_hit_ratio: 0.5,
      turns: 2, tool_calls: 3, assistant_messages: 4, usage_conflicts: 0, usage_schema_unknown: 0,
      context_tokens: 20000, cost_usd: null,
      time: { wall_secs: 100, active_secs: 50, turns_timed: 1, turn_mean_secs: 5, turn_median_secs: 5, turn_max_secs: 9, turn_source: "timestamps", wait_secs: null, wait_gaps: 0, wait_median_secs: null, wait_gaps_excluded: 0, pending_ask: false, clock_regressions: 0 },
    },
  },
  ...over,
});

beforeEach(() => {
  live.rows = [liveRow({ last_tool: "Edit", last_tool_at: NOW - 5, status: { model: "claude-opus-4-8" } })];
  cardMock.card = fullCard();
  convMock.data = undefined;
  convMock.calls = [];
  detailMock.detail = { project_slug: "demo" };
  boardMock.board = board({ ask: [slateCard(5, "ask", { to: "you", line: "question for you" })] });
  termsMock.terms = [term("t1"), term("t2", { harness: "omp" })];
  termsMock.isError = false;
  __resetMarks();
});
afterEach(cleanup);

describe("NowRail", () => {
  it("renders every section with its freshness label, in order", () => {
    render(<NowRail />, { wrapper: wrap(SID) });
    const ids = ["activity", "status", "answer", "asks", "siblings", "stats"];
    const secs = screen.getAllByTestId(/^now-sec-/).map((e) => e.getAttribute("data-testid"));
    expect(secs).toEqual(ids.map((i) => `now-sec-${i}`));
    for (const i of ids) expect(screen.getByTestId(`now-fresh-${i}`).textContent).not.toBe("");
    expect(screen.getByTestId("now-fresh-answer").textContent).toBe("as of last capture");
    expect(screen.getByTestId("now-answer").textContent).toBe("Final answer text");
    expect(screen.getByTestId("now-last-tool").textContent).toContain("Edit");
    expect(screen.getByTestId("now-subagents").textContent).toContain("2");
    expect(screen.getByTestId("now-asks-count").textContent).toContain("1 open ask");
    expect(within(screen.getByTestId("now-stats")).getByText(/Context/)).toBeTruthy();
    expect(screen.queryByTestId("now-sec-needs")).toBeNull();
  });

  it("shows the needs-you section with the pending question only when blocked", () => {
    live.rows = [liveRow({ blocked: true, blocked_since_unix: NOW - 90, detail_reason: "reason text" })];
    convMock.data = {
      kind: "ok",
      kb: "k",
      conversation: {
        capture_gen: `${NOW}-2`,
        rows: [{ asks: [{ kind: "ask", q: "Pick one?", options: [], answer: null, pending: true }] }],
      },
    };
    render(<NowRail />, { wrapper: wrap(SID) });
    expect(screen.getByTestId("now-needs-text").textContent).toBe("Pick one?");
    expect(screen.getByTestId("now-needs-age").textContent).toContain("1m ago");
    // The conversation is read only while blocked.
    expect(convMock.calls.at(-1)).toBe(SID);
  });

  it("does not read the conversation while the session is not blocked", () => {
    render(<NowRail />, { wrapper: wrap(SID) });
    expect(convMock.calls.every((c) => c === undefined)).toBe(true);
  });

  it("renders hostile strings as text, never markup", () => {
    const evil = '<img src=x onerror="alert(1)">';
    live.rows = [liveRow({ blocked: true, blocked_since_unix: NOW - 5, detail_reason: evil, last_line: evil, last_tool: evil })];
    cardMock.card = fullCard({ outcome: { available: true, last_assistant_text: evil } });
    boardMock.board = board({ ask: [slateCard(5, "ask", { to: "you", line: evil })] });
    termsMock.terms = [term("t1"), term("t2", { harness: evil })];
    const { container } = render(<NowRail />, { wrapper: wrap(SID) });
    expect(container.querySelector("img")).toBeNull();
    expect(screen.getByTestId("now-needs-text").textContent).toBe(evil);
    expect(screen.getByTestId("now-answer").textContent).toBe(evil);
    expect(screen.getByTestId("now-ask").textContent).toBe(evil);
  });

  it("an unbound terminal shows the calm state, still lists siblings, and never fetches a card", () => {
    cardMock.card = null;
    render(<NowRail />, { wrapper: wrap(undefined) });
    expect(screen.getByText("Not linked to a session yet")).toBeTruthy();
    expect(screen.queryByTestId("now-sec-activity")).toBeNull();
    expect(screen.getByTestId("now-sec-siblings")).toBeTruthy();
  });

  it("a sibling shows its attention mark and a click navigates to that terminal", () => {
    setMarks({ t2: { asks: true, finishedUnseen: false } });
    render(<NowRail />, { wrapper: wrap(SID) });
    const sib = screen.getByTestId("now-sibling");
    expect(sib.getAttribute("data-mark")).toBe("asks");
    fireEvent.click(sib);
    expect(screen.getByTestId("loc").textContent).toBe("/terminals/t2");
  });

  it("hides the siblings section when the terminal list is unreadable", () => {
    termsMock.isError = true;
    render(<NowRail />, { wrapper: wrap(SID) });
    expect(screen.queryByTestId("now-sec-siblings")).toBeNull();
  });

  it("carries the empty, hidden queue seam for PQUI", () => {
    const { container } = render(<NowRail />, { wrapper: wrap(SID) });
    const seam = container.querySelector('[data-testid="now-queue"]') as HTMLElement;
    expect(seam.getAttribute("data-seam")).toBe("pqui");
    expect(seam.hidden).toBe(true);
    expect(seam.childElementCount).toBe(0);
    expect(seam.textContent).toBe("");
  });

  it("shows no cost for a session whose harness reported none", () => {
    render(<NowRail />, { wrapper: wrap(SID) });
    expect(document.querySelector('[data-stat="cost"]')).toBeNull();
  });
});

describe("useNowBadge", () => {
  it("is 1 only while the bound session is blocked", () => {
    live.rows = [liveRow({ blocked: true })];
    const on = renderHook(() => useNowBadge(), { wrapper: wrap(SID) });
    expect(on.result.current).toBe(1);
    live.rows = [liveRow()];
    const off = renderHook(() => useNowBadge(), { wrapper: wrap(SID) });
    expect(off.result.current).toBeUndefined();
    const unbound = renderHook(() => useNowBadge(), { wrapper: wrap(undefined) });
    expect(unbound.result.current).toBeUndefined();
  });
});
