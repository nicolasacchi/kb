import { test, expect, type Page } from "@playwright/test";
import { BASE } from "./helpers";
import { installFakeTerminal, SID, TERM_ID } from "./fixtures/fake-tap.mjs";

// CONVRAIL — the Conversation tab of the terminal's right panel. The daemon's
// view and session-detail routes are mocked (synthetic rows only); the
// session.captured event that drives the refetch is the real one, from seeding
// a synthetic capture. CHROMIUM desktop only.

const row = (n: number, over: Record<string, unknown> = {}) => ({
  n,
  id: `t-0000000000${n}${n}`,
  ts: null,
  prompt: `synthetic prompt ${n}`,
  answer: `synthetic answer ${n}`,
  asks: [],
  commits: [],
  files_touched: 0,
  errors: 0,
  subagents: { n: 0, errors: 0 },
  ...over,
});

const ROWS3 = [
  row(1, { commits: ["abc1234def"] }),
  row(2, {
    asks: [{ kind: "ask", q: "Synthetic question two?", options: ["yes", "no"], answer: "yes", pending: false }],
  }),
  row(3, {
    answer: null,
    asks: [{ kind: "ask", q: "Synthetic pending question?", options: [], answer: null, pending: true }],
  }),
];

async function mockConversation(page: Page, state: { rows: unknown[]; gen: string }): Promise<void> {
  await page.route(`**/api/sessions/${SID}/view*`, async (route) => {
    const url = new URL(route.request().url());
    if (url.searchParams.get("fields") !== "conversation") return route.fallback();
    const unchanged = url.searchParams.get("conv_gen") === state.gen;
    await route.fulfill({
      status: 200,
      contentType: "application/json",
      body: JSON.stringify({
        grammar: "session-view/1",
        session_id: SID,
        kb: "mem",
        artifact_id: "synthetic",
        turns_total: state.rows.length,
        conversation: {
          schema: "conversation/1",
          total: state.rows.length,
          from: unchanged ? null : 1,
          next_cursor: null,
          capture_gen: state.gen,
          unchanged,
          rows: unchanged ? [] : state.rows,
        },
      }),
    });
  });
  await page.route(new RegExp(`/api/sessions/${SID}$`), (route) =>
    route.fulfill({
      status: 200,
      contentType: "application/json",
      body: JSON.stringify({ session_id: SID, kb: "mem", source_relative: "sessions/synthetic-conversation.html" }),
    }),
  );
}

test.describe("CONVRAIL Conversation tab", () => {
  test("rows, pending highlight, filter, jump link, caption; a capture event refetches", async ({ page, request }) => {
    await installFakeTerminal(page, { total: 300, seedLines: 100 });
    const nowSec = Math.floor(Date.now() / 1000);
    const state = { rows: ROWS3 as unknown[], gen: `${nowSec - 120}-3` };
    await mockConversation(page, state);

    await page.goto(`${BASE}/term/${TERM_ID}`);
    const rows = page.locator('[data-testid="conv-row"]');
    await expect(rows).toHaveCount(3, { timeout: 20_000 });
    await expect(rows.nth(2).locator('[data-pending="true"]')).toHaveText("Pending");
    await expect(rows.nth(2)).toContainText("Synthetic pending question?");
    await expect(page.locator('[data-testid="conv-fresh"]')).toContainText("As of last capture");

    await page.locator('[data-testid="conv-filter-questions"]').click();
    await expect(rows).toHaveCount(2);
    await page.locator('[data-testid="conv-filter-all"]').click();
    await expect(rows).toHaveCount(3);

    const href = await rows.nth(1).locator('[data-testid="conv-jump"]').getAttribute("href");
    expect(href).toContain("?turn=t-");
    expect(href).toContain("/a/mem/sessions/synthetic-conversation.html");

    // A new capture: the mocked projection now holds a 4th turn under a new gen.
    state.rows = [...ROWS3, row(4)];
    state.gen = `${nowSec}-4`;
    const seed = await request.post(`${BASE}/api/kb/mem/artifacts`, {
      data: {
        title: `Session transcript 20260701T0900${Date.now() % 10000}Z`,
        body_html: `<pre>${[
          `{"type":"user","timestamp":"2026-07-01T09:00:00Z","cwd":"/work/synthetic","message":{"role":"user","content":"synthetic prompt"}}`,
          `{"type":"assistant","timestamp":"2026-07-01T09:05:00Z","message":{"role":"assistant","content":[{"type":"text","text":"synthetic closing"}]}}`,
        ].join("\n")}</pre>`,
        category: "memory-session",
        session_id: SID,
      },
    });
    expect(seed.ok()).toBeTruthy();
    await expect(rows).toHaveCount(4, { timeout: 30_000 });
    await expect(rows.nth(3)).toContainText("synthetic prompt 4");
  });
});
