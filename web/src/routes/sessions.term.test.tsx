// @vitest-environment jsdom
//
// BSHELL - a terminal lives at /terminals/:id. The Terminals list's Open is a link
// that PUSHES the page, and the legacy `/sessions?term=<id>` deep link
// REDIRECTS there with `replace` (Back never lands on the old URL); an invalid
// `?term=` value is ignored.
import { afterEach, describe, expect, it, vi } from "vitest";
import { cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { MemoryRouter, Route, Routes, useLocation, useNavigate } from "react-router-dom";
import SessionsRoute from "./sessions";

vi.mock("../api/sse", () => ({
  sse: {
    subscribeEvent: () => () => {},
    onResync: () => () => {},
  },
}));

let admitted: boolean | undefined = true;
vi.mock("../terminal/TerminalPanel.lazy", () => ({
  __esModule: true,
  default: (p: { termId: string; onClose?: () => void }) => (
    <div data-testid="mock-panel" data-term={p.termId}>
      <button type="button" data-testid="mock-panel-close" onClick={() => p.onClose?.()}>
        Close
      </button>
    </div>
  ),
}));
vi.mock("../hooks/useArtifactHost", async (orig) => {
  const real = await orig<typeof import("../hooks/useArtifactHost")>();
  return {
    ...real,
    useIdentity: () => (admitted === undefined ? null : ({ terminal_admitted: admitted } as never)),
  };
});

vi.mock("../hooks/useSessions", () => ({
  useSessions: () => ({
    rows: [],
    loading: false,
    loadingMore: false,
    error: null,
    refresh: () => {},
    loadMore: () => {},
    hasMore: false,
  }),
  useSessionPresence: () => ({ presenceSet: new Set<string>(), enabled: false }),
  useSessionDetail: () => ({
    detail: null,
    memories: [],
    recalls: [],
    touches: null,
    readings: [],
    files: [],
    decisions: [],
    commits: [],
    research: [],
    comments: { artifacts: [], total: 0, raised: [] },
    loading: false,
  }),
  useSessionFolders: () => [],
  useSessionProjects: () => ({ projects: [], loading: false }),
  useSessionRecollect: () => ({ hits: [], loading: false }),
  useSessionThreads: () => ({ threads: [], loading: false }),
  useSessionWork: () => ({
    work: { schema: "kb-session-work/1", since: 0, scan_capped: false, truncated: false, groups: [], skipped: [] },
    loading: false,
  }),
}));

vi.mock("../hooks/useLiveSessions", () => ({
  useLiveSessions: () => ({ rows: [] }),
}));

afterEach(() => {
  cleanup();
  vi.unstubAllGlobals();
});

const TERM = {
  id: "t1",
  harness: "claude",
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

function stubTerms() {
  vi.stubGlobal(
    "fetch",
    vi.fn(async (url: string | URL) =>
      String(url).includes("/api/terms")
        ? new Response(JSON.stringify({ terms: [TERM] }), {
            status: 200,
            headers: { "Content-Type": "application/json" },
          })
        : new Response("{}", { status: 404 }),
    ),
  );
}

function Probe() {
  const loc = useLocation();
  const nav = useNavigate();
  return (
    <div>
      <span data-testid="loc">{loc.pathname + loc.search}</span>
      <button type="button" data-testid="back" onClick={() => nav(-1)}>
        back
      </button>
    </div>
  );
}

function mountAt(entries: string[], index?: number) {
  const qc = new QueryClient({ defaultOptions: { queries: { retry: false } } });
  return render(
    <QueryClientProvider client={qc}>
      <MemoryRouter initialEntries={entries} initialIndex={index}>
        <Probe />
        <Routes>
          <Route path="/sessions" element={<SessionsRoute />} />
          <Route path="/terminals/:id" element={<div data-testid="term-page" />} />
        </Routes>
      </MemoryRouter>
    </QueryClientProvider>,
  );
}
const loc = () => screen.getByTestId("loc").textContent;

describe("/sessions -> /terminals/:id (BSHELL)", () => {
  it("Open pushes /terminals/<id>; Back returns to the list", async () => {
    stubTerms();
    mountAt(["/sessions"]);
    fireEvent.click(await screen.findByTestId("term-row-open"));
    await screen.findByTestId("term-page");
    expect(loc()).toBe("/terminals/t1");
    fireEvent.click(screen.getByTestId("back"));
    await waitFor(() => expect(loc()).toBe("/sessions"));
  });

  it("the legacy ?term=<id> link redirects with replace: Back skips it", async () => {
    stubTerms();
    mountAt(["/sessions?q=before", "/sessions?term=t1"], 1);
    await screen.findByTestId("term-page");
    expect(loc()).toBe("/terminals/t1");
    fireEvent.click(screen.getByTestId("back"));
    // One Back reaches the entry BEFORE the legacy link, not a leftover ?term=t1.
    await waitFor(() => expect(loc()).toBe("/sessions?q=before"));
  });

  it("an invalid ?term= value is ignored: no redirect", async () => {
    stubTerms();
    mountAt(["/sessions?term=BAD/.."]);
    await screen.findByTestId("term-row");
    expect(screen.queryByTestId("term-page")).toBeNull();
    expect(loc()).toBe("/sessions?term=BAD/..");
  });
});
