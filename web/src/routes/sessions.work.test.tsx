// @vitest-environment jsdom
//
// A6 — the work view: pieces of work by the fixed named ladder.
import { afterEach, describe, expect, it, vi } from "vitest";
import { cleanup, render, screen } from "@testing-library/react";
import { MemoryRouter } from "react-router-dom";
import { SessionWorkView } from "./sessions";
import { sessionsUrl } from "../lib/sessionsUrl";
import { loadLastSessionsView } from "../api/prefs";
import type { WorkResponse } from "../api/sessions";

// The route module pulls many hooks; in-memory SSE keeps this suite
// network-free (same facade mock as sessions.row.test.tsx).
vi.mock("../api/sse", () => ({
  sse: {
    subscribeEvent: () => () => {},
    onResync: () => () => {},
  },
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
    expect(screen.getByText("Root work")).toBeTruthy();
    expect(screen.getByText(/subagent tree · 3 sessions · 2 subagents/)).toBeTruthy();
    const link = screen.getByText("Root work").closest("a");
    expect(link).toBeTruthy();
    expect(link!.getAttribute("href")).toBe("/sessions/root-x");
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

  it("the remembered view is never 'work'", () => {
    // The route's setView persists ONLY projects/list (D3); 'work' (like
    // 'threads') flows through the same setParams path but leaves the
    // remembered view untouched.
    expect(["projects", "list"]).toContain(loadLastSessionsView());
    expect(loadLastSessionsView()).not.toBe("work");
  });
});
