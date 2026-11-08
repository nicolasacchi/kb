// @vitest-environment jsdom
//
// A6 — the work view: pieces of work by the fixed named ladder.
import { afterEach, describe, expect, it, vi } from "vitest";
import { cleanup, fireEvent, render, screen } from "@testing-library/react";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { MemoryRouter, Route, Routes } from "react-router-dom";
import SessionsRoute, { SessionWorkView } from "./sessions";
import { sessionsUrl } from "../lib/sessionsUrl";
import { loadLastSessionsView, saveLastSessionsView } from "../api/prefs";
import type { WorkResponse } from "../api/sessions";

// The route module pulls many hooks; in-memory SSE keeps this suite
// network-free (same facade mock as sessions.row.test.tsx).
vi.mock("../api/sse", () => ({
  sse: {
    subscribeEvent: () => () => {},
    onResync: () => () => {},
  },
}));

// Quiet-empty data hooks so the REAL SessionsRoute renders its own
// view-switching (setView + persisted view) without any network.
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

function renderWork(work: WorkResponse, loading = false) {
  return render(
    <MemoryRouter>
      <SessionWorkView work={work} loading={loading} />
    </MemoryRouter>,
  );
}

const EMPTY_WORK: WorkResponse = {
  schema: "kb-session-work/1",
  since: 0,
  scan_capped: false,
  truncated: false,
  groups: [],
  skipped: [],
};

describe("SessionWorkView", () => {
  it("renders a tree group: caption, counts, member link, +N more", () => {
    const work: WorkResponse = {
      ...EMPTY_WORK,
      groups: [
        {
          key: "smoke:root-x",
          term: "tree",
          terms: ["tree"],
          started_at: 100,
          last_activity: 500,
          members: [
            {
              kb: "smoke",
              session_id: "root-x",
              role: "root",
              via: "tree",
              started_at: 100,
              ended_at: 500,
              harness: "omp",
              title: "Root work",
            },
          ],
          members_total: 3,
          counts: { logical_sessions: 3, subagents: 2, parts: 3, active_secs: 400 },
        },
      ],
    };
    renderWork(work);
    expect(screen.getAllByText("Root work").length).toBeGreaterThan(0);
    expect(screen.getByText(/subagent tree · 3 sessions · 2 subagents/)).toBeTruthy();
    const link = screen.getByRole("link", { name: "Root work" });
    expect(link.getAttribute("href")).toBe("/sessions/root-x");
    // members_total 3 vs 1 shown member.
    expect(screen.getByText("+2 more")).toBeTruthy();
  });

  it("renders an alone group and the skipped caption", () => {
    const work: WorkResponse = {
      ...EMPTY_WORK,
      groups: [
        {
          key: "smoke:lone",
          term: "alone",
          terms: [],
          started_at: 700,
          last_activity: 800,
          members: [
            {
              kb: "smoke",
              session_id: "lone",
              role: "member",
              via: "alone",
              started_at: 700,
              ended_at: 800,
              harness: "claude",
            },
          ],
          members_total: 1,
          counts: { logical_sessions: 1, subagents: 0, parts: 1, active_secs: 100 },
        },
      ],
      skipped: [
        { term: "job", reason: "fanout_cap", detail: "ulid-big on 51 sessions" },
      ],
    };
    renderWork(work);
    expect(screen.getByText(/alone · 1 session · 0 subagents/)).toBeTruthy();
    const skipped = screen.getByTestId("work-skipped");
    expect(skipped.textContent).toContain("ulid-big on 51 sessions");
    expect(skipped.textContent).toContain("fanout_cap");
  });

  it("renders an empty state when there are no groups", () => {
    renderWork(EMPTY_WORK);
    expect(screen.getByText("no pieces of work yet")).toBeTruthy();
  });

  it("sessionsUrl emits view=work", () => {
    expect(sessionsUrl({ view: "work" })).toBe("/sessions?view=work");
  });

  it("clicking the Work toggle leaves loadLastSessionsView() unchanged", () => {
    // The route's setView persists ONLY projects/list (D3): selecting work
    // writes ?view=work (and renders the work view) but must not persist it.
    saveLastSessionsView("projects");
    expect(loadLastSessionsView()).toBe("projects");
    const qc = new QueryClient({ defaultOptions: { queries: { retry: false } } });
    render(
      <QueryClientProvider client={qc}>
        <MemoryRouter initialEntries={["/sessions?view=projects"]}>
          <Routes>
            <Route path="/sessions" element={<SessionsRoute />} />
          </Routes>
        </MemoryRouter>
      </QueryClientProvider>,
    );
    fireEvent.click(screen.getByTestId("sessions-view-work"));
    // The toggle took effect (work view is showing its empty state) …
    expect(screen.getByText("no pieces of work yet")).toBeTruthy();
    // … but the remembered landing view did not move.
    expect(loadLastSessionsView()).toBe("projects");
  });
});
