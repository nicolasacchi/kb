import { test, expect, type Page } from "@playwright/test";
import { BASE } from "./helpers";
import { installFakeTerminal, SID, TERM_ID } from "./fixtures/fake-tap.mjs";

// CONVNEED — the needs-you banner above the pane. The live-status and the
// conversation reads are mocked (synthetic text only); the banner derives from
// those two reads and nothing else. CHROMIUM desktop only: the phone layout is
// pinned by the TermShell jsdom tests.

const SINCE = 1_760_000_000;

type Live = { blocked: boolean; reason?: string; tool?: string };

async function admit(page: Page): Promise<void> {
  await page.route("**/api/identity", async (route) => {
    const res = await route.fetch();
    const j = (await res.json()) as Record<string, unknown>;
    await route.fulfill({ response: res, json: { ...j, terminal_admitted: true } });
  });
}

async function mockLive(page: Page, live: Live): Promise<void> {
  await page.route("**/api/sessions/live-status", (r) =>
    r.fulfill({
      status: 200,
      contentType: "application/json",
      body: JSON.stringify({
        sessions: [
          {
            session_id: SID,
            harness: "claude",
            holder: "agent",
            state: "working",
            source: "hook",
            confidence: "observed",
            since_unix: SINCE,
            since_secs: 10,
            resume: "",
            term: TERM_ID,
            blocked: live.blocked,
            ...(live.blocked ? { blocked_since_unix: SINCE, ...(live.reason ? { detail_reason: live.reason } : {}), ...(live.tool ? { blocked_tool: live.tool } : {}) } : {}),
          },
        ],
      }),
    }),
  );
}

const row = (n: number, pending: boolean) => ({
  n,
  id: `t-0000000000${String(n).padStart(2, "0")}`,
  ts: null,
  prompt: `synthetic prompt ${n} `.repeat(6),
  answer: pending ? null : `synthetic answer ${n} `.repeat(6),
  asks: pending ? [{ kind: "ask", q: "Synthetic pending question?", options: ["alpha", "beta"], answer: null, pending: true }] : [],
  commits: [],
  files_touched: 0,
  errors: 0,
  subagents: { n: 0, errors: 0 },
});

async function mockConversation(page: Page, gen: string): Promise<void> {
  const rows = Array.from({ length: 24 }, (_, i) => row(i + 1, i === 23));
  await page.route(`**/api/sessions/${SID}/view*`, async (route) => {
    const url = new URL(route.request().url());
    if (url.searchParams.get("fields") !== "conversation") return route.fallback();
    await route.fulfill({
      status: 200,
      contentType: "application/json",
      body: JSON.stringify({
        grammar: "session-view/1",
        session_id: SID,
        kb: "mem",
        artifact_id: "synthetic",
        turns_total: rows.length,
        conversation: { schema: "conversation/1", total: rows.length, from: 1, next_cursor: null, capture_gen: gen, unchanged: false, rows },
      }),
    });
  });
  await page.route(new RegExp(`/api/sessions/${SID}$`), (route) =>
    route.fulfill({
      status: 200,
      contentType: "application/json",
      body: JSON.stringify({ session_id: SID, kb: "mem", source_relative: "sessions/synthetic-needs.html" }),
    }),
  );
}

test.describe("CONVNEED needs-you banner", () => {
  test("beat text while blocked above the pane, gone when unblocked", async ({ page }) => {
    const live: Live = { blocked: false };
    await installFakeTerminal(page, { total: 300, seedLines: 100 });
    await admit(page);
    // A capture older than the block: its pending question must never show.
    await mockConversation(page, `${SINCE - 100}-24`);
    await mockLive(page, live);
    await page.goto(`${BASE}/terminals/${TERM_ID}`);
    const pane = page.locator(".kb-termshell__pane");
    await expect(pane).toBeVisible({ timeout: 20_000 });
    await expect(page.locator('[data-testid="needs-you"]')).toHaveCount(0);

    live.blocked = true;
    live.reason = "Synthetic permission needed";
    live.tool = "Bash";
    await page.evaluate(() => window.dispatchEvent(new Event("focus")));
    const banner = page.locator('[data-testid="needs-you"]');
    await expect(banner).toBeVisible({ timeout: 25_000 });
    await expect(banner).toHaveAttribute("data-source", "beat");
    await expect(page.locator('[data-testid="needs-you-text"]')).toHaveText("Synthetic permission needed");
    await expect(page.locator('[data-testid="needs-you-tool"]')).toHaveText("Bash");
    await expect(banner).not.toContainText("Synthetic pending question?");
    // The banner is its own row above the pane, never an overlay on it.
    const bb = (await banner.boundingBox())!;
    const pb = (await pane.boundingBox())!;
    expect(bb.y + bb.height).toBeLessThanOrEqual(pb.y + 1);

    live.blocked = false;
    await page.evaluate(() => window.dispatchEvent(new Event("focus")));
    await expect(banner).toHaveCount(0, { timeout: 25_000 });
  });

  test("a fresh capture supplies the question, and the jump focuses the pending row", async ({ page }) => {
    await installFakeTerminal(page, { total: 300, seedLines: 100 });
    await admit(page);
    await mockConversation(page, `${SINCE + 50}-24`);
    await mockLive(page, { blocked: true, reason: "Synthetic permission needed" });
    await page.goto(`${BASE}/terminals/${TERM_ID}`);
    const banner = page.locator('[data-testid="needs-you"]');
    await expect(banner).toHaveAttribute("data-source", "capture", { timeout: 25_000 });
    await expect(page.locator('[data-testid="needs-you-text"]')).toHaveText("Synthetic pending question?");
    await expect(page.locator('[data-testid="needs-you-options"]')).toHaveText("alpha / beta");

    await page.locator('[data-testid="term-tab-changes"]').click();
    await page.locator('[data-testid="needs-you-jump"]').click();
    await expect(page.locator('[data-testid="term-tab-conversation"]')).toHaveAttribute("aria-selected", "true");
    const pendingRow = page.locator('[data-testid="conv-row"]').filter({ has: page.locator('[data-pending="true"]') });
    await expect(pendingRow).toBeFocused({ timeout: 10_000 });
    await expect(pendingRow).toBeInViewport();
  });
});
