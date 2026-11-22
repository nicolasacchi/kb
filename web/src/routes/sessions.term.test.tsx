// @vitest-environment jsdom
//
// FUMINLIST - the `?term=` history semantics on /sessions: opening a terminal
// from the list PUSHES one entry (Back closes it), closing REPLACES it (Back
// never re-opens a closed sheet), and an unknown id is a REPLACE too (no extra
// entry is left behind for Back to land on).
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
        </Routes>
      </MemoryRouter>
    </QueryClientProvider>,
  );
}
const loc = () => screen.getByTestId("loc").textContent;

describe("/sessions ?term= history (FUMINLIST)", () => {
  it("Open pushes one entry: the URL gains ?term= and Back closes the sheet", async () => {
    stubTerms();
    mountAt(["/sessions"]);
    fireEvent.click(await screen.findByTestId("term-row-open"));
    await screen.findByTestId("mock-panel");
    expect(loc()).toBe("/sessions?term=t1");
    fireEvent.click(screen.getByTestId("back"));
    await waitFor(() => expect(loc()).toBe("/sessions"));
    await waitFor(() => expect(screen.queryByTestId("mock-panel")).toBeNull());
  });

  it("closing the sheet replaces its entry: Back afterwards does not re-open it", async () => {
    stubTerms();
    mountAt(["/sessions?q=prev"]);
    fireEvent.click(await screen.findByTestId("term-row-open"));
    await screen.findByTestId("mock-panel");
    expect(loc()).toBe("/sessions?q=prev&term=t1");
    fireEvent.click(screen.getByTestId("mock-panel-close"));
    await waitFor(() => expect(loc()).toBe("/sessions?q=prev"));
    fireEvent.click(screen.getByTestId("back"));
    // The pushed entry was replaced by the close, so Back lands on the entry
    // before it (the page without ?term=), never on ?term=t1 again.
    await waitFor(() => expect(loc()).toBe("/sessions?q=prev"));
    expect(screen.queryByTestId("mock-panel")).toBeNull();
  });

  it("an unknown ?term= id is a replace: no extra entry for Back to land on", async () => {
    stubTerms();
    mountAt(["/sessions?q=before", "/sessions?term=zzz9"], 1);
    await screen.findByTestId("term-not-found");
    expect(loc()).toBe("/sessions");
    fireEvent.click(screen.getByTestId("back"));
    // One Back reaches the entry BEFORE the bad link, not a leftover ?term=zzz9.
    await waitFor(() => expect(loc()).toBe("/sessions?q=before"));
  });
});

