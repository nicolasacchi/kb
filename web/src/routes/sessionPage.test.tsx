// @vitest-environment jsdom
//
// A4 — the /sessions/:sid page against fixture cards (the real kb-term
// lane does not exist yet, so the terminal slot is only exercised by its
// invariant test). Each behaviour must fail without the change it pins.
import { afterEach, describe, expect, it, vi } from "vitest";
import { cleanup, render, screen, waitFor } from "@testing-library/react";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { MemoryRouter, Route, Routes } from "react-router-dom";
import SessionPage from "./sessionPage";
import type { SessionDetail } from "../api/sessions";
const SID = "abcdef0123456789";

const DETAIL = {
  id: "d1",
  kb: "demo",
  artifact_id: "art-1",
  session_id: SID,
  started_at: 1_700_000_000,
  ended_at: 1_700_003_600,
  duration_ms: 3_600_000,
  message_count: 12,
  memory_count: 2,
  source_relative: "transcript.html",
  display_name: "Build the widget",
  harness: "claude",
  checks: [],
  unnamed_edits: [],
  unnamed_edits_total: 0,
  memory_ids: [],
  evidence: {
    verdict: "verified",
    phrase: "cargo test passed after the last edit",
    deciding: undefined,
  },
} as unknown as SessionDetail;

function fullCard(overrides: Record<string, unknown> = {}) {
  return {
    schema: "kb-session-card/1",
    session_id: SID,
    kb: "demo",
    artifact_id: "art-1",
    text: "summary text",
    summary: {
      available: true,
      display_name: "Build the widget",
      harness: "claude",
      substance: "routine",
      active_secs: 3600,
      started_at: 1_700_000_000,
      ended_at: 1_700_003_600,
      segment_chain: [SID, "second-part"],
      child_total: 2,
      resume: "omp -r ROOT-SID",
    },
    outcome: {
      available: true,
      last_assistant_text: "Shipped the widget.",
      verdict: "verified",
      checks: [],
      not_committed: {
        files: ["src/main.ts"],
        commits_considered: 2,
        commits_total: 2,
        capped: true,
        unresolved_commits: 0,
        caption: "checked the newest 2 of 2 commits; files may be committed earlier",
      },
    },
    files: {
      available: true,
      files: [
        { path: "src/main.ts", action: "write", via_subagent: false },
        { path: "src/lib.ts", action: "edit", via_subagent: false },
        { path: "README.md", action: "read", via_subagent: false },
      ],
      total: 3,
      read: 1,
      edited: 2,
      wrote: 1,
    },
    decisions: {
      available: true,
      decisions: [{ kind: "plan_approval", prompt: "Ship it?", answer: "yes" }],
      total: 1,
    },
    commits: {
      available: true,
      commits: [{ kind: "commit", sha: "abc1234", subject: "feat: widget", resolved: true }],
      total: 1,
    },
    research: { available: true, kind_counts: {}, items: [], total: 0 },
    comments: {
      available: true,
      open_count: 1,
      raised_count: 1,
      open: [
        {
          comment_id: "c1",
          kb: "demo",
          artifact_id: "a1",
          author: "operator",
          body: "look at this",
          file_label: "src/main.ts",
          created_at: 1_700_000_000,
          status: "open",
          touched: true,
        },
      ],
      raised: [],
    },
    readings: {
      available: true,
      items: [
        {
          kb: "demo",
          artifact_id: "a1",
          title: "Doc",
          source_relative: "doc.html",
          opened_at: 1_700_000_000,
          read_pct: 50,
        },
      ],
      total: 1,
    },
    memory: {
      available: true,
      produced: ["mem-1"],
      produced_total: 1,
      recalled: [{ kb: "demo", id: "mem-9", title: "Past fix", used: true, pos: 3 }],
    },
    live: { available: false, reason: "not live" },
    slate: {
      available: true,
      slug: "kb",
      head_seq: 42,
      posts: [{ seq: 42, id: "p1", at: 1_700_000_000, kind: "found", line: "found the bug" }],
      total: 1,
    },
    children: {
      available: true,
      total: 2,
      items: [
        {
          session_id: "child-1",
          display_name: "Child one",
          started_at: 1_700_000_010,
          ended_at: 1_700_000_100,
          harness: "claude",
          files_edited_count: 5,
          tool_calls: 7,
        },
      ],
      next_after: "1700000010:child-1",
    },
    terminal: { available: false, reason: "no terminal runner" },
    ...overrides,
  };
}

function mockFetch(opts: {
  card: unknown;
  cardStatus?: number;
  childrenPage?: (url: string) => unknown;
}) {
  return vi.fn(async (url: unknown) => {
    const u = String(url);
    if (u.endsWith("/card")) {
      const status = opts.cardStatus ?? 200;
      return {
        ok: status === 200,
        status,
        statusText: status === 200 ? "OK" : "Not Found",
        json: async () => opts.card,
      } as Response;
    }
    if (u.includes("/children")) {
      return {
        ok: true,
        status: 200,
        statusText: "OK",
        json: async () =>
          opts.childrenPage?.(u) ?? {
            session_id: SID,
            total: 0,
            items: [],
            next_after: null,
          },
      } as Response;
    }
    if (u.endsWith(`/api/sessions/${SID}`)) {
      return {
        ok: true,
        status: 200,
        statusText: "OK",
        json: async () => DETAIL,
      } as Response;
    }
    return {
      ok: false,
      status: 404,
      statusText: "Not Found",
      json: async () => ({}),
    } as Response;
  });
}

function renderPage() {
  const qc = new QueryClient({
    defaultOptions: { queries: { retry: false } },
  });
  return render(
    <QueryClientProvider client={qc}>
      <MemoryRouter initialEntries={[`/sessions/${SID}`]}>
        <Routes>
          <Route path="/sessions/:sid" element={<SessionPage />} />
        </Routes>
      </MemoryRouter>
    </QueryClientProvider>,
  );
}

afterEach(cleanup);

describe("SessionPage", () => {
  it("renders every section in the spec-3 order", async () => {
    vi.stubGlobal("fetch", mockFetch({ card: fullCard() }));
    const { container } = renderPage();
    await waitFor(() =>
      expect(screen.getByTestId("session-sec-outcome")).toBeTruthy(),
    );
    const ids = [...container.querySelectorAll('[data-testid^="session-sec-"]')].map(
      (el) => el.getAttribute("data-testid"),
    );
    expect(ids).toEqual([
      "session-sec-outcome",
      "session-sec-transcript",
      "session-sec-children",
      "session-sec-files",
      "session-sec-decisions",
      "session-sec-commits",
      "session-sec-memory",
      "session-sec-slate",
      "session-sec-comments",
      "session-sec-segments",
      "session-sec-resume",
    ]);
  });

  it("an unavailable files lane shows its reason while outcome still renders", async () => {
    vi.stubGlobal(
      "fetch",
      mockFetch({
        card: fullCard({
          files: {
            available: false,
            reason: "denied for this fixture",
            files: [],
            total: 0,
            read: 0,
            edited: 0,
            wrote: 0,
          },
        }),
      }),
    );
    renderPage();
    await waitFor(() =>
      expect(screen.getByTestId("session-sec-files")).toBeTruthy(),
    );
    expect(screen.getByTestId("session-sec-files").textContent).toContain(
      "denied for this fixture",
    );
    expect(screen.getByTestId("session-page-outcome-text").textContent).toBe(
      "Shipped the widget.",
    );
  });

  it("load more fetches the next children page by cursor and appends in order", async () => {
    const fetchFn = mockFetch({
      card: fullCard(),
      childrenPage: () => ({
        session_id: SID,
        total: 2,
        items: [
          {
            session_id: "child-2",
            display_name: "Child two",
            started_at: 1_700_000_200,
            ended_at: 1_700_000_300,
            harness: "claude",
            files_edited_count: 1,
            tool_calls: 2,
          },
        ],
        next_after: null,
      }),
    });
    vi.stubGlobal("fetch", fetchFn);
    renderPage();
    await waitFor(() =>
      expect(screen.getByTestId("session-children-total")).toBeTruthy(),
    );
    expect(screen.getByTestId("session-children-total").textContent).toBe("2 total");
    expect(screen.getByText("Child one")).toBeTruthy();
    screen.getByTestId("session-children-more").click();
    await waitFor(() => expect(screen.getByText("Child two")).toBeTruthy());
    const childrenUrl = fetchFn.mock.calls
      .map((c) => String(c[0]))
      .find((u) => u.includes("/children"));
    expect(childrenUrl).toBeTruthy();
    const after = new URL(childrenUrl!, "http://x").searchParams.get("after");
    expect(after).toBe("1700000010:child-1");
  });

  it("keeps the parent files count as its own — never summed with a child", async () => {
    vi.stubGlobal("fetch", mockFetch({ card: fullCard() }));
    renderPage();
    await waitFor(() =>
      expect(screen.getByTestId("session-files-total")).toBeTruthy(),
    );
    expect(screen.getByTestId("session-files-total").textContent).toContain("3 files");
    const childRow = screen.getByTestId("session-child-row");
    expect(childRow.textContent).toContain("5 edited");
    expect(screen.getByTestId("session-files-total").textContent).not.toContain("8");
  });

  it("renders the server's resume string verbatim and builds no claude command", async () => {
    vi.stubGlobal("fetch", mockFetch({ card: fullCard() }));
    const { container } = renderPage();
    await waitFor(() =>
      expect(screen.getByTestId("session-sec-resume")).toBeTruthy(),
    );
    expect(container.textContent).toContain("omp -r ROOT-SID");
    expect(container.textContent).not.toContain("claude -r");
  });

  it("a 404 card renders a StateCard with the server's reason", async () => {
    vi.stubGlobal(
      "fetch",
      mockFetch({
        card: { title: "Not Found", detail: "session missing" },
        cardStatus: 404,
      }),
    );
    const { container } = renderPage();
    await waitFor(() => expect(
      container.querySelector(".kb-statecard"),
    ).toBeTruthy());
    expect(container.textContent).toContain("Could not load session");
    expect(container.textContent).toContain("Not Found: session missing");
  });
});
