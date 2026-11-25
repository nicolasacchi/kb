// @vitest-environment jsdom
//
// BSHELL — /terminals/:id: admission, unknown id, list error, the found mount and
// the close navigation.
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { MemoryRouter, Route, Routes, useLocation } from "react-router-dom";

let admitted: boolean | null = true;
vi.mock("../hooks/useArtifactHost", async (orig) => {
  const real = await orig<typeof import("../hooks/useArtifactHost")>();
  return {
    ...real,
    useIdentity: () => (admitted === null ? null : ({ terminal_admitted: admitted } as never)),
  };
});
vi.mock("../components/ConfirmProvider", () => ({
  useConfirm: () => async () => true,
  useConfirmOptional: () => async () => true,
}));
vi.mock("../terminal/TerminalPanel.lazy", () => ({
  __esModule: true,
  default: (p: { termId: string; sid: string; harness: string; onClose?: () => void; headerExtra?: React.ReactNode }) => (
    <div data-testid="mock-panel" data-term={p.termId} data-sid={p.sid} data-harness={p.harness}>
      <div data-testid="mock-panel-extra">{p.headerExtra}</div>
      <button type="button" data-testid="mock-panel-close" onClick={() => p.onClose?.()}>
        Close
      </button>
    </div>
  ),
}));

import TermPageRoute from "./termPage";

const ROW = {
  id: "t1",
  harness: "omp",
  cwd: "/srv/work/alpha",
  started_unix: 1,
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
};

let listStatus = 200;
let listRows: object[] = [ROW];

function Where() {
  const l = useLocation();
  return <div data-testid="where">{l.pathname}</div>;
}

function StateProbe() {
  const l = useLocation();
  return <div data-testid="state-probe">{JSON.stringify(l.state)}</div>;
}

function mountAt(path: string) {
  const qc = new QueryClient({ defaultOptions: { queries: { retry: false } } });
  return render(
    <QueryClientProvider client={qc}>
      <MemoryRouter initialEntries={[path]}>
        <Routes>
          <Route path="/terminals/:id" element={<TermPageRoute />} />
          <Route path="*" element={<Where />} />
        </Routes>
      </MemoryRouter>
    </QueryClientProvider>,
  );
}

beforeEach(() => {
  admitted = true;
  listStatus = 200;
  listRows = [ROW];
  vi.stubGlobal("matchMedia", (q: string) =>
    ({ matches: false, media: q, addEventListener() {}, removeEventListener() {}, addListener() {}, removeListener() {} }) as unknown as MediaQueryList,
  );
  vi.stubGlobal(
    "fetch",
    vi.fn(async () =>
      listStatus === 200
        ? new Response(JSON.stringify({ terms: listRows }), { status: 200, headers: { "Content-Type": "application/json" } })
        : new Response(JSON.stringify({ title: "Bad Gateway", detail: "down" }), {
            status: listStatus,
            headers: { "Content-Type": "application/json" },
          }),
    ),
  );
});
afterEach(() => {
  cleanup();
  vi.unstubAllGlobals();
});

describe("/terminals/:id", () => {
  it("not admitted: a card, no panel", async () => {
    admitted = false;
    mountAt("/terminals/t1");
    expect(await screen.findByText("Terminals are not enabled")).toBeTruthy();
    expect(screen.queryByTestId("mock-panel")).toBeNull();
  });

  it("an invalid id: not-found with a link to /sessions", async () => {
    mountAt("/terminals/BAD..");
    const nf = await screen.findByTestId("term-not-found");
    expect(nf.querySelector("a")?.getAttribute("href")).toBe("/sessions");
  });

  it("a loaded list without the id: not-found", async () => {
    mountAt("/terminals/zzz");
    const nf = await screen.findByTestId("term-not-found");
    expect(nf.querySelector("a")?.getAttribute("href")).toBe("/sessions");
    expect(screen.queryByTestId("mock-panel")).toBeNull();
  });

  it("a list error keeps the id, shows retry, and does not say not found", async () => {
    listStatus = 502;
    mountAt("/terminals/t1");
    const err = await screen.findByTestId("term-list-error");
    expect(err.textContent).toContain("t1");
    expect(screen.queryByTestId("term-not-found")).toBeNull();
    listStatus = 200;
    fireEvent.click(screen.getByRole("button", { name: "Retry" }));
    await screen.findByTestId("mock-panel");
    expect(screen.queryByTestId("term-list-error")).toBeNull();
  });

  it("found: the shell wraps the panel, and the title names harness and directory", async () => {
    const prev = document.title;
    const { unmount } = mountAt("/terminals/t1");
    const p = await screen.findByTestId("mock-panel");
    expect(p.getAttribute("data-harness")).toBe("omp");
    expect(screen.getByTestId("term-shell").contains(p)).toBe(true);
    expect(document.title).toContain("omp");
    expect(document.title).toContain("alpha");
    unmount();
    expect(document.title).toBe(prev);
  });

  it("desktop: exactly one End, in the shell header, none in the panel's headerExtra", async () => {
    mountAt("/terminals/t1");
    await screen.findByTestId("mock-panel");
    const ends = screen.getAllByTestId("kb-term-end");
    expect(ends).toHaveLength(1);
    expect(ends[0].closest(".kb-termshell__head")).toBeTruthy();
    expect(screen.getByTestId("mock-panel-extra").querySelector('[data-testid="kb-term-end"]')).toBeNull();
  });

  it("phone: exactly one End, in the panel's headerExtra, none in the shell header", async () => {
    vi.stubGlobal(
      "matchMedia",
      (q: string) =>
        ({ matches: true, media: q, addEventListener() {}, removeEventListener() {}, addListener() {}, removeListener() {} }) as unknown as MediaQueryList,
    );
    mountAt("/terminals/t1");
    await screen.findByTestId("mock-panel");
    const ends = screen.getAllByTestId("kb-term-end");
    expect(ends).toHaveLength(1);
    expect(screen.getByTestId("mock-panel-extra").contains(ends[0])).toBe(true);
    expect(document.querySelector(".kb-termshell__head")?.querySelector('[data-testid="kb-term-end"]') ?? null).toBeNull();
  });

  it("a launch hint is consumed: it mounts the panel once, then history.state is cleared", async () => {
    listRows = [];
    const qc = new QueryClient({ defaultOptions: { queries: { retry: false } } });
    render(
      <QueryClientProvider client={qc}>
        <MemoryRouter initialEntries={[{ pathname: "/terminals/t9", state: { launched: { id: "t9", harness: "omp" } } }]}>
          <Routes>
            <Route path="/terminals/:id" element={<><TermPageRoute /><StateProbe /></>} />
          </Routes>
        </MemoryRouter>
      </QueryClientProvider>,
    );
    expect(await screen.findByTestId("mock-panel")).toBeTruthy();
    await waitFor(() => expect(screen.getByTestId("state-probe").textContent).toBe("null"));
    expect(screen.getByTestId("mock-panel")).toBeTruthy();
  });

  it("Close goes to the bound session, else to /sessions", async () => {
    listRows = [{ ...ROW, session_id: "s-9" }];
    mountAt("/terminals/t1");
    fireEvent.click(await screen.findByTestId("mock-panel-close"));
    await waitFor(() => expect(screen.getByTestId("where").textContent).toBe("/sessions/s-9"));
    cleanup();
    listRows = [ROW];
    mountAt("/terminals/t1");
    fireEvent.click(await screen.findByTestId("mock-panel-close"));
    await waitFor(() => expect(screen.getByTestId("where").textContent).toBe("/sessions"));
  });
});
