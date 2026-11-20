// @vitest-environment jsdom
//
// A4 — the /sessions/:sid page against fixture cards (the real kb-term
// lane does not exist yet, so the terminal slot is only exercised by its
// invariant test). Each behaviour must fail without the change it pins.
import { afterEach, describe, expect, it, vi } from "vitest";
import { cleanup, render, screen, waitFor } from "@testing-library/react";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { MemoryRouter, Route, Routes } from "react-router-dom";
import SessionPage, { ChildrenSection } from "./sessionPage";
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
    terminal: {
      available: false,
      reason: "terminal disabled",
      admitted: false,
    },
    ...overrides,
  };
}

function mockFetch(opts: {
  card: unknown;
  cardStatus?: number;
  childrenPage?: (url: string) => unknown;
  withCode?: boolean;
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
    if (u.includes("/api/kbs")) {
      return {
        ok: true,
        status: 200,
        statusText: "OK",
        json: async () =>
          opts.withCode
            ? [{ name: "demo", code_url: "https://code.example.test" }]
            : [],
      } as Response;
    }
    if (u.includes("/api/session-card")) {
      return new Response(
        JSON.stringify({
          schema: "kbc-session-card/1",
          sid: "root-0001",
          now_unix: 1_700_000_000,
          commits: {
            available: true,
            total: 0,
            shown: 0,
            truncated: false,
            items: [],
          },
          reviews: { available: true, total: 0, shown: 0, items: [] },
          findings: {
            available: true,
            total: 0,
            by_severity: { blocker: 0, concern: 0, ok: 0 },
            basis: "author",
          },
          presence: { available: true, files: 0, newest_age_secs: 0 },
        }),
        {
          status: 200,
          headers: { "Content-Type": "application/json" },
        },
      );
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
    // T21: the Images section rides a lazy chunk; wait for it to mount.
    await waitFor(() =>
      expect(screen.getByTestId("session-sec-images")).toBeTruthy(),
    );
    const ids = [...container.querySelectorAll('[data-testid^="session-sec-"]')].map(
      (el) => el.getAttribute("data-testid"),
    );
    expect(ids).toEqual([
      "session-sec-outcome",
      "session-sec-transcript",
      "session-sec-images",
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
    // The rows come from the children query, which resolves after mount;
    // `session-children-total` renders immediately from the card lane.
    await screen.findByText("Child one");
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
    // The child row comes from the children query, which resolves after
    // mount; the files total renders immediately from the card lane.
    const childRow = await screen.findByTestId("session-child-row");
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
    // The loading StateCard matches `.kb-statecard` immediately, so wait
    // for the degraded card's title, which only renders once the failed
    // card fetch has rejected (retry is disabled on this client).
    await waitFor(() =>
      expect(container.textContent).toContain("Could not load session"),
    );
    expect(container.textContent).toContain("Not Found: session missing");
  });
  it("hides the segments section when the chain has fewer than two entries", async () => {
    for (const chain of [[], [SID]]) {
      cleanup();
      const card = fullCard();
      card.summary.segment_chain = chain;
      vi.stubGlobal("fetch", mockFetch({ card }));
      renderPage();
      await waitFor(() =>
        expect(screen.getByTestId("session-sec-resume")).toBeTruthy(),
      );
      expect(screen.queryByTestId("session-sec-segments")).toBeNull();
    }
  });

  it("renders no terminal section while the lane is available but unadmitted", async () => {
    vi.stubGlobal(
      "fetch",
      mockFetch({ card: fullCard({ terminal: { available: true, admitted: false } }) }),
    );
    renderPage();
    await waitFor(() =>
      expect(screen.getByTestId("session-sec-resume")).toBeTruthy(),
    );
    expect(screen.queryByTestId("session-sec-terminal")).toBeNull();
    expect(screen.queryByTestId("terminal-slot")).toBeNull();
  });

  it("uses icons — not unicode glyphs — for the back and transcript links", async () => {
    vi.stubGlobal("fetch", mockFetch({ card: fullCard() }));
    const { container } = renderPage();
    await waitFor(() =>
      expect(screen.getByTestId("session-sec-resume")).toBeTruthy(),
    );
    const back = screen.getByTestId("session-back");
    expect(back.querySelector("svg")).toBeTruthy();
    expect(container.textContent).not.toContain("←");
    expect(container.textContent).not.toContain("→");
  });

  it("T14: renders the terminal section for an available+admitted live lane", async () => {
    vi.stubGlobal(
      "fetch",
      mockFetch({
        card: fullCard({
          terminal: { available: true, admitted: true, term_id: "abc123", state: "working" },
        }),
      }),
    );
    renderPage();
    await waitFor(() =>
      expect(screen.getByTestId("session-sec-resume")).toBeTruthy(),
    );
    expect(screen.getByTestId("session-sec-terminal")).toBeTruthy();
  });

  it("FXA4: capped-scan caption renders verbatim when the file list is empty", async () => {
    vi.stubGlobal(
      "fetch",
      mockFetch({
        card: fullCard({
          outcome: {
            available: true,
            last_assistant_text: "Shipped.",
            verdict: "verified",
            checks: [],
            not_committed: {
              files: [],
              commits_considered: 20,
              commits_total: 25,
              capped: true,
              unresolved_commits: 0,
              caption: "checked the newest 20 of 25 commits",
            },
          },
        }),
      }),
    );
    renderPage();
    await waitFor(() =>
      expect(screen.getByTestId("session-sec-outcome")).toBeTruthy(),
    );
    expect(screen.getByTestId("session-page-notcommitted-caption").textContent).toBe(
      "checked the newest 20 of 25 commits",
    );
  });

  it("FXA4: a children load-more failure shows an error caption, not silence", async () => {
    vi.stubGlobal(
      "fetch",
      vi.fn(async (url: unknown) => {
        const u = String(url);
        if (u.endsWith("/card")) {
          return { ok: true, status: 200, statusText: "OK", json: async () => fullCard() } as Response;
        }
        if (u.includes("/children")) {
          return {
            ok: false,
            status: 500,
            statusText: "Internal Server Error",
            json: async () => ({ title: "Boom", detail: "children down" }),
          } as Response;
        }
        if (u.endsWith(`/api/sessions/${SID}`)) {
          return { ok: true, status: 200, statusText: "OK", json: async () => DETAIL } as Response;
        }
        return { ok: false, status: 404, statusText: "Not Found", json: async () => ({}) } as Response;
      }),
    );
    renderPage();
    await waitFor(() =>
      expect(screen.getByTestId("session-children-total")).toBeTruthy(),
    );
    await screen.findByText("Child one");
    screen.getByTestId("session-children-more").click();
    await waitFor(() =>
      expect(screen.getByTestId("session-children-error")).toBeTruthy(),
    );
    expect(screen.getByTestId("session-children-error").textContent).toContain(
      "Boom: children down",
    );
  });

  it("FXA4: load-more sends limit=50 alongside the after cursor", async () => {
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
    await screen.findByText("Child one");
    screen.getByTestId("session-children-more").click();
    await waitFor(() => expect(screen.getByText("Child two")).toBeTruthy());
    const childrenUrl = fetchFn.mock.calls
      .map((c) => String(c[0]))
      .find((u) => u.includes("/children"));
    const params = new URL(childrenUrl!, "http://x").searchParams;
    expect(params.get("after")).toBe("1700000010:child-1");
    expect(params.get("limit")).toBe("50");
  });

  it("FXA4: a lane change after page 2 resets pagination (no stale cursor)", async () => {
    // Direct ChildrenSection test: load page 2, then swap the lane object
    // (as a bridge-invalidated card refetch does) and assert page 0 re-seeds
    // with the new first page instead of keeping the stale cursor.
    const lane1 = fullCard().children as {
      available: boolean; total: number;
      items: { session_id: string; display_name: string; started_at: number; ended_at: number; harness: string; files_edited_count: number; tool_calls: number }[];
      next_after: string;
    };
    const lane2 = {
      available: true,
      total: 3,
      items: [{
        session_id: "child-9",
        display_name: "Child nine",
        started_at: 1_700_000_900,
        ended_at: 1_700_001_000,
        harness: "claude",
        files_edited_count: 1,
        tool_calls: 1,
      }],
      next_after: undefined as string | undefined,
    };
    vi.stubGlobal(
      "fetch",
      vi.fn(async (url: unknown) => {
        const u = String(url);
        if (u.includes("/children")) {
          return {
            ok: true, status: 200, statusText: "OK",
            json: async () => ({
              session_id: SID, total: 2,
              items: [{
                session_id: "child-2", display_name: "Child two",
                started_at: 1_700_000_200, ended_at: 1_700_000_300,
                harness: "claude", files_edited_count: 1, tool_calls: 2,
              }],
              next_after: null,
            }),
          } as Response;
        }
        return { ok: false, status: 404, statusText: "Not Found", json: async () => ({}) } as Response;
      }),
    );
    const qc = new QueryClient({ defaultOptions: { queries: { retry: false } } });
    const { rerender } = render(
      <QueryClientProvider client={qc}>
        <MemoryRouter initialEntries={[`/sessions/${SID}`]}>
          <Routes>
            <Route path="/sessions/:sid" element={<ChildrenSection sid={SID} lane={lane1} open />} />
          </Routes>
        </MemoryRouter>
      </QueryClientProvider>,
    );
    await screen.findByText("Child one");
    screen.getByTestId("session-children-more").click();
    await waitFor(() => expect(screen.getByText("Child two")).toBeTruthy());
    // Lane changes: the generation in the query key must reset pagination.
    rerender(
      <QueryClientProvider client={qc}>
        <MemoryRouter initialEntries={[`/sessions/${SID}`]}>
          <Routes>
            <Route path="/sessions/:sid" element={<ChildrenSection sid={SID} lane={lane2 as never} open />} />
          </Routes>
        </MemoryRouter>
      </QueryClientProvider>,
    );
    await waitFor(() => expect(screen.getByText("Child nine")).toBeTruthy());
    expect(screen.queryByText("Child two")).toBeNull();
  });

  it("FXA4: zero-commits card shows an explicit caption, not an empty section", async () => {
    vi.stubGlobal(
      "fetch",
      mockFetch({
        card: fullCard({
          commits: { available: true, commits: [], total: 0 },
        }),
      }),
    );
    renderPage();
    await waitFor(() =>
      expect(screen.getByTestId("session-sec-commits")).toBeTruthy(),
    );
    expect(screen.getByTestId("session-commits-empty").textContent).toContain(
      "no commits",
    );
  });

  it("FXA4: research and live lanes are intentionally not sections", async () => {
    vi.stubGlobal("fetch", mockFetch({ card: fullCard() }));
    renderPage();
    await waitFor(() =>
      expect(screen.getByTestId("session-sec-resume")).toBeTruthy(),
    );
    expect(screen.queryByTestId("session-sec-research")).toBeNull();
    expect(screen.queryByTestId("session-sec-live")).toBeNull();
  });

  it("FXA4: a failed detail fetch still shows the replay link plus an unavailable caption", async () => {
    vi.stubGlobal(
      "fetch",
      vi.fn(async (url: unknown) => {
        const u = String(url);
        if (u.endsWith("/card")) {
          return { ok: true, status: 200, statusText: "OK", json: async () => fullCard() } as Response;
        }
        // Detail endpoint fails (500 -> fetchSession returns null).
        if (u.endsWith(`/api/sessions/${SID}`)) {
          return {
            ok: false,
            status: 500,
            statusText: "Internal Server Error",
            json: async () => ({}),
          } as Response;
        }
        if (u.includes("/children")) {
          return {
            ok: true, status: 200, statusText: "OK",
            json: async () => ({ session_id: SID, total: 0, items: [], next_after: null }),
          } as Response;
        }
        return { ok: false, status: 404, statusText: "Not Found", json: async () => ({}) } as Response;
      }),
    );
    renderPage();
    await waitFor(() =>
      expect(screen.getByTestId("session-sec-transcript")).toBeTruthy(),
    );
    expect(screen.getByTestId("session-transcript-replay")).toBeTruthy();
    await waitFor(() =>
      expect(screen.getByTestId("session-transcript-unavailable")).toBeTruthy(),
    );
    expect(screen.queryByTestId("session-transcript-reader")).toBeNull();
  });

  it("FXA4: non-JSON 500 card surfaces the status line as the StateCard reason", async () => {
    vi.stubGlobal(
      "fetch",
      vi.fn(async (url: unknown) => {
        const u = String(url);
        if (u.endsWith("/card")) {
          return {
            ok: false,
            status: 500,
            statusText: "Internal Server Error",
            json: async (): Promise<never> => {
              throw new SyntaxError("Unexpected token <");
            },
          } as unknown as Response;
        }
        return { ok: false, status: 404, statusText: "Not Found", json: async () => ({}) } as Response;
      }),
    );
    const { container } = renderPage();
    await waitFor(() =>
      expect(container.textContent).toContain("Could not load session"),
    );
    expect(container.textContent).toContain("500 Internal Server Error");
  });

  it("FXA4: invalid-JSON success card surfaces an error StateCard (no crash)", async () => {
    vi.stubGlobal(
      "fetch",
      vi.fn(async (url: unknown) => {
        const u = String(url);
        if (u.endsWith("/card")) {
          return {
            ok: true,
            status: 200,
            statusText: "OK",
            json: async (): Promise<never> => {
              throw new SyntaxError("Unexpected token <");
            },
          } as unknown as Response;
        }
        return { ok: false, status: 404, statusText: "Not Found", json: async () => ({}) } as Response;
      }),
    );
    const { container } = renderPage();
    await waitFor(() =>
      expect(container.textContent).toContain("Could not load session"),
    );
  });

  it("passes the root sid (not the child or parent) to the code lane", async () => {
    // A5 contract: for a subagent child the SPA passes the ROOT sid
    // (kb-code does no root resolution; trailer attribution stamps the
    // MAIN sid). A mutant passing the raw page sid, or `treeTarget`'s
    // parent fallback, queries the wrong session and the commits lane
    // is empty on every child page.
    const baseCard = fullCard();
    const summary = {
      ...baseCard.summary,
      root_session_id: "root-0001",
      parent_session_id: "parent-0001",
    };
    const fetchFn = mockFetch({ card: fullCard({ summary }), withCode: true });
    vi.stubGlobal("fetch", fetchFn);
    renderPage();
    await waitFor(() =>
      expect(screen.getByTestId("session-sec-resume")).toBeTruthy(),
    );
    // Wait until the code-lane query fires, then inspect its URL.
    await waitFor(() =>
      expect(
        fetchFn.mock.calls
          .map((c) => String(c[0]))
          .some((u) => u.includes("/api/session-card")),
      ).toBe(true),
    );
    const codeLaneUrls = fetchFn.mock.calls
      .map((c) => String(c[0]))
      .filter((u) => u.includes("/api/session-card"));
    expect(codeLaneUrls.length).toBeGreaterThan(0);
    expect(codeLaneUrls[0]).toContain("sid=root-0001");
    expect(codeLaneUrls[0]).not.toContain(`sid=${SID}`);
    expect(codeLaneUrls[0]).not.toContain("parent-0001");
  });
});

// FUWEBOPEN — a session known only to the live registry (no capture yet).
function liveOnlyCard(overrides: Record<string, unknown> = {}) {
  const none = { available: false, reason: "no capture yet" };
  return {
    schema: "kb-session-card/1",
    session_id: SID,
    kb: "",
    artifact_id: "",
    capture_pending: true,
    text: "session text",
    summary: {
      available: true,
      display_name: "demo",
      harness: "claude",
      project_slug: "demo",
      active_secs: 0,
      started_at: 0,
      ended_at: 0,
      segment_chain: [],
      child_total: 0,
      resume: `claude -r ${SID}`,
    },
    outcome: { ...none, verdict: "not_extracted", checks: [], not_committed: null },
    files: { ...none, files: [], total: 0, read: 0, edited: 0, wrote: 0 },
    decisions: { ...none, decisions: [], total: 0 },
    commits: { ...none, commits: [], total: 0 },
    research: none,
    comments: none,
    readings: none,
    memory: none,
    live: { available: true, state: "waiting", holder: "human", since_secs: 12, blocked: false },
    slate: none,
    children: { ...none, total: 0, items: [] },
    terminal: {
      available: true,
      admitted: true,
      term_id: "abc123",
      harness: "claude",
      state: "waiting",
    },
    ...overrides,
  };
}

function renderLiveOnly(entry = `/sessions/${SID}`) {
  const qc = new QueryClient({ defaultOptions: { queries: { retry: false } } });
  const utils = render(
    <QueryClientProvider client={qc}>
      <MemoryRouter initialEntries={[entry]}>
        <Routes>
          <Route path="/sessions/:sid" element={<SessionPage />} />
        </Routes>
      </MemoryRouter>
    </QueryClientProvider>,
  );
  return { qc, ...utils };
}

describe("SessionPage — live-only card (FUWEBOPEN)", () => {
  it("renders the header, the pending caption, the Terminal and Resume sections, and nothing capture-backed", async () => {
    const f = mockFetch({ card: liveOnlyCard() });
    vi.stubGlobal("fetch", f);
    const { container, qc } = renderLiveOnly();
    await waitFor(() => expect(screen.getByTestId("session-capture-pending")).toBeTruthy());
    expect(screen.getByTestId("session-live-state").textContent).toContain("waiting");
    expect(screen.getByTestId("session-sec-terminal")).toBeTruthy();
    expect(screen.getByTestId("kb-terminal-slot")).toBeTruthy();
    const ids = [...container.querySelectorAll('[data-testid^="session-sec-"]')].map((el) =>
      el.getAttribute("data-testid"),
    );
    expect(ids).toEqual(["session-sec-terminal", "session-sec-resume"]);
    // Once the card says live-only the detail bundle (which would only 404
    // for a session with no capture) is held off: a bridge invalidation of
    // the ["session"] prefix (session.state / slate.updated) refetches
    // nothing. (The very first paint may still fire it in parallel with the
    // card, as for every session, before the card has said what it is.)
    f.mockClear();
    await qc.invalidateQueries({ queryKey: ["session"] });
    const urls = f.mock.calls.map((c) => String(c[0]));
    expect(urls.some((u) => u.endsWith(`/api/sessions/${SID}`))).toBe(false);
  });

  it("a live-only card with no terminal shows no Terminal section", async () => {
    vi.stubGlobal(
      "fetch",
      mockFetch({
        card: liveOnlyCard({
          terminal: { available: false, reason: "not running in kb-term", admitted: true },
        }),
      }),
    );
    renderLiveOnly();
    await waitFor(() => expect(screen.getByTestId("session-capture-pending")).toBeTruthy());
    expect(screen.queryByTestId("session-sec-terminal")).toBeNull();
  });

  it("leaves the live-only view by itself when the card query is invalidated and a capture exists", async () => {
    let current: unknown = liveOnlyCard();
    const inner = mockFetch({ card: null });
    vi.stubGlobal(
      "fetch",
      vi.fn(async (url: unknown) => {
        if (String(url).endsWith("/card")) {
          return {
            ok: true,
            status: 200,
            statusText: "OK",
            json: async () => current,
          } as Response;
        }
        return inner(url);
      }),
    );
    const { qc } = renderLiveOnly();
    await waitFor(() => expect(screen.getByTestId("session-capture-pending")).toBeTruthy());
    expect(screen.queryByTestId("session-sec-outcome")).toBeNull();
    // The first capture lands; the bridge's session.captured handler
    // invalidates exactly this key (pinned in queryClient.test.ts).
    current = fullCard();
    await qc.invalidateQueries({ queryKey: ["sessions", "card", SID] });
    await waitFor(() => expect(screen.getByTestId("session-sec-outcome")).toBeTruthy());
    expect(screen.queryByTestId("session-capture-pending")).toBeNull();
  });

  describe("#sec-terminal", () => {
    afterEach(() => vi.unstubAllGlobals());

    function mobile() {
      vi.stubGlobal("matchMedia", (q: string) => ({
        matches: true,
        media: q,
        addEventListener: () => {},
        removeEventListener: () => {},
        addListener: () => {},
        removeListener: () => {},
      }));
    }

    it("opens the Terminal section on mobile (sections otherwise collapsed) and scrolls to it", async () => {
      mobile();
      const scroll = vi.fn();
      Element.prototype.scrollIntoView = scroll;
      vi.stubGlobal("fetch", mockFetch({ card: liveOnlyCard() }));
      renderLiveOnly(`/sessions/${SID}#sec-terminal`);
      const sec = await screen.findByTestId("session-sec-terminal");
      expect((sec as HTMLDetailsElement).open).toBe(true);
      // The resume section (not the target) stays collapsed on mobile.
      expect((screen.getByTestId("session-sec-resume") as HTMLDetailsElement).open).toBe(false);
      await waitFor(() => expect(scroll).toHaveBeenCalled());
    });

    it("also opens it on the full page, and stays closed without the hash on mobile", async () => {
      mobile();
      vi.stubGlobal("fetch", mockFetch({ card: fullCard({ terminal: { available: true, admitted: true, term_id: "abc123" } }) }));
      renderLiveOnly(`/sessions/${SID}#sec-terminal`);
      const sec = await screen.findByTestId("session-sec-terminal");
      expect((sec as HTMLDetailsElement).open).toBe(true);
      cleanup();
      renderLiveOnly(`/sessions/${SID}`);
      const closed = await screen.findByTestId("session-sec-terminal");
      expect((closed as HTMLDetailsElement).open).toBe(false);
    });
  });
});
