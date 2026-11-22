// @vitest-environment jsdom
//
// FUPHONEMODE (GAPS-9) - the /sessions page mounts the "New terminal" launcher
// only for an admitted caller: `identity.terminal_admitted` gates it at the
// page level, not just inside the component.
import { afterEach, describe, expect, it, vi } from "vitest";
import { cleanup, render, screen } from "@testing-library/react";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { MemoryRouter, Route, Routes } from "react-router-dom";
import SessionsRoute from "./sessions";

vi.mock("../api/sse", () => ({
  sse: {
    subscribeEvent: () => () => {},
    onResync: () => () => {},
  },
}));

let admitted: boolean | undefined;
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

afterEach(cleanup);

function mountPage() {
  const qc = new QueryClient({ defaultOptions: { queries: { retry: false } } });
  return render(
    <QueryClientProvider client={qc}>
      <MemoryRouter initialEntries={["/sessions"]}>
        <Routes>
          <Route path="/sessions" element={<SessionsRoute />} />
        </Routes>
      </MemoryRouter>
    </QueryClientProvider>,
  );
}

describe("the /sessions page gates the launcher on terminal_admitted", () => {
  it("terminal_admitted true: the New terminal control is mounted", () => {
    admitted = true;
    mountPage();
    expect(screen.getByTestId("new-terminal-open")).toBeTruthy();
  });
  it("terminal_admitted false: no launcher control", () => {
    admitted = false;
    mountPage();
    expect(screen.queryByTestId("new-terminal")).toBeNull();
    expect(screen.queryByTestId("new-terminal-open")).toBeNull();
  });
  it("identity not loaded yet: no launcher control (fail closed)", () => {
    admitted = undefined;
    mountPage();
    expect(screen.queryByTestId("new-terminal")).toBeNull();
  });
});
