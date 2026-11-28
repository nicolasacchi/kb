// @vitest-environment jsdom
//
// HUBDOOR — /terminals, the hub list page, through the real page with mocked
// data hooks.
import { afterEach, beforeAll, beforeEach, describe, expect, it, vi } from "vitest";
import { act, cleanup, fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { MemoryRouter, Route, Routes, useLocation, useNavigate } from "react-router-dom";
import type { TermRow } from "../api/terms";
import type { LiveStatusRow } from "../api/generated/LiveStatusRow";

const m = vi.hoisted(() => ({
  identity: { terminal_admitted: true } as { terminal_admitted: boolean } | null,
  terms: { terms: [] as unknown[], isLoading: false, isError: false },
  live: { rows: [] as unknown[], loading: false },
}));
const confirmMock = vi.hoisted(() => vi.fn(async () => false));
vi.mock("../components/ConfirmProvider", () => ({
  useConfirm: () => confirmMock,
  useConfirmOptional: () => confirmMock,
}));
vi.mock("../hooks/useArtifactHost", () => ({ useIdentity: () => m.identity }));
vi.mock("../terminal/useTerms", () => ({ useTerms: () => m.terms }));
vi.mock("../hooks/useLiveSessions", () => ({ useLiveSessions: () => m.live }));
vi.mock("../hooks/useNowTick", () => ({ useNowTick: () => 1_000_000_000_000 }));

import TerminalsPageRoute from "./terminalsPage";
import { __resetMarks, setMarks } from "../terminal/notify/attention";

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
  return (
    <>
      <div data-testid="where">{l.pathname + l.search}</div>
      <div data-testid="where-state">{JSON.stringify(l.state)}</div>
    </>
  );
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

type Call = { method: string; url: string; body: unknown };
let calls: Call[];
let details: Record<string, { status: number; body: unknown }>;
const posts = () => calls.filter((c) => c.method === "POST");

beforeAll(() => {
  if (!HTMLDialogElement.prototype.showModal) {
    HTMLDialogElement.prototype.showModal = function (this: HTMLDialogElement) {
      this.open = true;
    };
  }
});

beforeEach(() => {
  calls = [];
  details = {};
  vi.stubGlobal(
    "fetch",
    vi.fn(async (input: string | URL, init?: RequestInit) => {
      const url = String(input);
      const method = init?.method ?? "GET";
      calls.push({ method, url, body: init?.body ? JSON.parse(String(init.body)) : null });
      const json = (status: number, body: unknown) =>
        new Response(JSON.stringify(body), { status, headers: { "Content-Type": "application/json" } });
      if (method === "POST") return json(200, { id: "t9" });
      const d = /^\/api\/sessions\/([^/?]+)$/.exec(url);
      if (d) {
        const hit = details[decodeURIComponent(d[1])];
        return hit ? json(hit.status, hit.body) : json(404, {});
      }
      if (url.startsWith("/api/sessions")) return json(200, { sessions: [] });
      return json(200, { terms: [] });
    }),
  );
  m.identity = { terminal_admitted: true };
  m.terms = { terms: [], isLoading: false, isError: false };
  m.live = { rows: [], loading: false };
});
afterEach(() => {
  cleanup();
  vi.unstubAllGlobals();
});

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

const sessionRow = (o: Record<string, unknown> = {}) => ({
  id: "i",
  session_id: "sid1",
  harness: "codex",
  cwd: "/p/x",
  started_at: 1_700_000_000,
  display_name: "Fix it",
  project_key: "proj",
  ...o,
});

describe("New session (HUBHOST)", () => {
  it("closing the dialog returns focus to the New session button", async () => {
    mount();
    const btn = screen.getByTestId("hub-new");
    fireEvent.click(btn);
    fireEvent.click(screen.getByTestId("launch-close"));
    await waitFor(() => expect(document.activeElement).toBe(btn));
  });

  it("each row has an End/Remove button that does not navigate", () => {
    m.terms = {
      isLoading: false,
      isError: false,
      terms: [term("a"), term("dead", { pane_dead: true })],
    };
    mount();
    const btns = screen.getAllByTestId("kb-term-end");
    expect(btns.map((b) => b.textContent).sort()).toEqual(["End", "Remove"]);
    fireEvent.click(btns[0]);
    expect(screen.getByTestId("where").textContent).toBe("/terminals");
  });

  it("is in the header and opens the launcher in the empty state", () => {
    mount();
    expect(screen.getByTestId("hub-empty")).toBeTruthy();
    expect(screen.queryByTestId("launch-modal")).toBeNull();
    fireEvent.click(screen.getByTestId("hub-new"));
    expect(screen.getByTestId("launch-modal")).toBeTruthy();
    expect(posts()).toHaveLength(0);
  });

  it("also works while loading, on a list error and with rows", () => {
    m.terms = { terms: [], isLoading: true, isError: false };
    mount();
    fireEvent.click(screen.getByTestId("hub-new"));
    expect(screen.getByTestId("launch-modal")).toBeTruthy();
    cleanup();
    m.terms = { terms: [], isLoading: false, isError: true };
    mount();
    fireEvent.click(screen.getByTestId("hub-new"));
    expect(screen.getByTestId("launch-modal")).toBeTruthy();
    cleanup();
    m.terms = { isLoading: false, isError: false, terms: [term("a")] };
    mount();
    fireEvent.click(screen.getByTestId("hub-new"));
    expect(screen.getByTestId("launch-modal")).toBeTruthy();
  });

  it("a not-admitted caller has no button", () => {
    m.identity = { terminal_admitted: false };
    mount();
    expect(screen.queryByTestId("hub-new")).toBeNull();
  });

  it("one POST on the Launch click, then lands on /terminals/<id> with the launch hint and invalidates [terms]", async () => {
    const { spy } = mount();
    fireEvent.click(screen.getByTestId("hub-new"));
    fireEvent.change(screen.getByTestId("launch-cwd"), { target: { value: "/tmp/x" } });
    fireEvent.click(screen.getByTestId("launch-review"));
    expect(screen.getByTestId("launch-confirm-cwd").textContent).toBe("Directory: /tmp/x");
    expect(posts()).toHaveLength(0);
    fireEvent.click(screen.getByTestId("launch-confirm"));
    await waitFor(() => expect(screen.getByTestId("where").textContent).toBe("/terminals/t9"));
    expect(posts()).toHaveLength(1);
    expect(posts()[0].body).toMatchObject({ cwd: "/tmp/x" });
    expect(JSON.parse(screen.getByTestId("where-state").textContent!)).toEqual({
      launched: { id: "t9", harness: (posts()[0].body as { harness: string }).harness },
    });
    expect(spy).toHaveBeenCalledWith({ queryKey: ["terms"] });
  });
});

describe("deep link ?launch=1&resume= (HUBHOST, H4)", () => {
  it("opens the dialog prefilled from the fetched row, strips the params with replace, and never POSTs on load", async () => {
    details.sid1 = { status: 200, body: sessionRow() };
    mount("/terminals?project=alpha&launch=1&resume=sid1");
    expect(screen.getByTestId("launch-modal")).toBeTruthy();
    await waitFor(() => expect(screen.getByTestId("where").textContent).toBe("/terminals?project=alpha"));
    // The dialog survives the strip.
    expect(screen.getByTestId("launch-modal")).toBeTruthy();
    await waitFor(() =>
      expect((screen.getByTestId("launch-review") as HTMLButtonElement).disabled).toBe(false),
    );
    fireEvent.click(screen.getByTestId("launch-review"));
    expect(screen.getByTestId("launch-confirm-cwd").textContent).toBe("Directory: /p/x");
    expect(screen.getByTestId("launch-confirm-harness").textContent).toBe("Harness: Codex");
    expect(posts()).toHaveLength(0);
    fireEvent.click(screen.getByTestId("launch-confirm"));
    await waitFor(() => expect(posts()).toHaveLength(1));
    expect(posts()[0].body).toEqual({ harness: "codex", cwd: "/p/x", resume_sid: "sid1" });
  });

  it("remounting at the stripped URL does not reopen it", async () => {
    details.sid1 = { status: 200, body: sessionRow() };
    mount("/terminals?launch=1&resume=sid1");
    await waitFor(() => expect(screen.getByTestId("where").textContent).toBe("/terminals"));
    cleanup();
    mount("/terminals");
    expect(screen.queryByTestId("launch-modal")).toBeNull();
  });

  it("Back after the strip does not land on the launch URL or reopen the dialog (replace, not push)", async () => {
    details.sid1 = { status: 200, body: sessionRow() };
    let nav: ReturnType<typeof useNavigate> | null = null;
    function Grab() {
      nav = useNavigate();
      return null;
    }
    const qc = new QueryClient();
    render(
      <QueryClientProvider client={qc}>
        <MemoryRouter initialEntries={["/terminals?x=1", "/terminals?launch=1&resume=sid1"]} initialIndex={1}>
          <Routes>
            <Route path="/terminals" element={<><TerminalsPageRoute /><Where /><Grab /></>} />
          </Routes>
        </MemoryRouter>
      </QueryClientProvider>,
    );
    await waitFor(() => expect(screen.getByTestId("where").textContent).toBe("/terminals"));
    fireEvent.click(screen.getByTestId("launch-close"));
    act(() => nav!(-1));
    expect(screen.getByTestId("where").textContent).toBe("/terminals?x=1");
    expect(screen.queryByTestId("launch-modal")).toBeNull();
  });

  it("in-page navigation to ?launch=1&resume= opens it once and strips it", async () => {
    details.sid1 = { status: 200, body: sessionRow() };
    let nav: ReturnType<typeof useNavigate> | null = null;
    function Grab() {
      nav = useNavigate();
      return null;
    }
    const qc = new QueryClient();
    render(
      <QueryClientProvider client={qc}>
        <MemoryRouter initialEntries={["/terminals"]}>
          <Routes>
            <Route path="/terminals" element={<><TerminalsPageRoute /><Where /><Grab /></>} />
          </Routes>
        </MemoryRouter>
      </QueryClientProvider>,
    );
    expect(screen.queryByTestId("launch-modal")).toBeNull();
    act(() => nav!("/terminals?launch=1&resume=sid1"));
    await waitFor(() => expect(screen.getByTestId("launch-modal")).toBeTruthy());
    await waitFor(() => expect(screen.getByTestId("where").textContent).toBe("/terminals"));
    expect(posts()).toHaveLength(0);
  });

  it("resume without launch is dropped by the grammar: nothing opens", () => {
    mount("/terminals?resume=sid1");
    expect(screen.queryByTestId("launch-modal")).toBeNull();
    expect(calls.filter((c) => c.url.startsWith("/api/sessions/sid1"))).toHaveLength(0);
  });

  // launch/resume are consumed and `cwd` is not a hub param; `harness` is a hub
  // FILTER param, so it is kept in the URL (and never reaches the dialog).
  it("launch=1 alone opens a blank dialog; harness and cwd in the URL are ignored", async () => {
    mount("/terminals?launch=1&harness=grok&cwd=/evil");
    expect(screen.getByTestId("launch-modal")).toBeTruthy();
    expect(screen.getByTestId("launch-new-form")).toBeTruthy();
    expect((screen.getByTestId("launch-cwd") as HTMLInputElement).value).toBe("");
    await waitFor(() => expect(screen.getByTestId("where").textContent).toBe("/terminals?harness=grok"));
    expect(posts()).toHaveLength(0);
  });

  it("a harness/cwd smuggled next to resume never reaches the confirm", async () => {
    details.sid1 = { status: 200, body: sessionRow() };
    mount("/terminals?launch=1&resume=sid1&harness=grok&cwd=/evil");
    await waitFor(() =>
      expect((screen.getByTestId("launch-review") as HTMLButtonElement).disabled).toBe(false),
    );
    fireEvent.click(screen.getByTestId("launch-review"));
    expect(screen.getByTestId("launch-confirm-cwd").textContent).toBe("Directory: /p/x");
    expect(screen.getByTestId("launch-confirm-harness").textContent).toBe("Harness: Codex");
  });

  it("an unknown sid shows the error and offers no launch", async () => {
    mount("/terminals?launch=1&resume=nope");
    await waitFor(() => expect(screen.getByTestId("launch-prefill-error")).toBeTruthy());
    expect((screen.getByTestId("launch-review") as HTMLButtonElement).disabled).toBe(true);
    expect(posts()).toHaveLength(0);
  });

  it("not admitted: no dialog and the URL is left alone", () => {
    m.identity = { terminal_admitted: false };
    mount("/terminals?launch=1&resume=sid1");
    expect(screen.queryByTestId("launch-modal")).toBeNull();
    expect(posts()).toHaveLength(0);
  });
});

describe("/terminals attention marks (HUBSWITCH)", () => {
  afterEach(() => act(() => __resetMarks()));

  it("each row shows its own asks / finished mark, distinct in text", () => {
    m.terms = {
      terms: [term("a", { cwd: "/w/one" }), term("b", { cwd: "/w/two" }), term("c", { cwd: "/w/three" })],
      isLoading: false,
      isError: false,
    };
    act(() =>
      setMarks({
        a: { asks: true, finishedUnseen: false },
        b: { asks: false, finishedUnseen: true },
      }),
    );
    mount();
    const row = (id: string) => document.querySelector(`[data-testid="hub-row"][data-term="${id}"]`) as HTMLElement;
    expect(within(row("a")).getByTestId("mark-asks").textContent).toContain("asks");
    expect(within(row("a")).queryByTestId("mark-finished")).toBeNull();
    expect(within(row("b")).getByTestId("mark-finished").textContent).toContain("finished");
    expect(within(row("b")).queryByTestId("mark-asks")).toBeNull();
    expect(within(row("c")).queryByTestId("attention-marks")).toBeNull();
  });
});
