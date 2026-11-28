import { test, expect, type Page } from "@playwright/test";
import { BASE } from "./helpers";
import { installFakeTerminal, SID, TERM_ID } from "./fixtures/fake-tap.mjs";

// RAILNOW - the Now tab (first tab of the terminal's right panel). The live
// status and the session card are mocked with synthetic values; unblocking the
// mocked row must remove the needs-you section and the badge on the next
// live-status poll. CHROMIUM desktop only.

const T0 = Math.floor(Date.now() / 1000);

function liveRow(blocked: boolean) {
  return {
    session_id: SID,
    harness: "claude",
    holder: "agent",
    state: "working",
    source: "hook",
    confidence: "observed",
    since_unix: T0 - 120,
    since_secs: 120,
    resume: "claude -r synthetic",
    blocked,
    ...(blocked ? { blocked_tool: "Bash", blocked_since_unix: T0 - 90, detail_reason: "synthetic permission prompt" } : {}),
    last_tool: "Edit",
    last_tool_at: T0 - 20,
    status: { model: "claude-opus-4-8", context_pct: 41 },
  };
}

const card = {
  schema: "kb-session-card/1",
  session_id: SID,
  outcome: { available: true, last_assistant_text: "synthetic final answer", verdict: "not_extracted", checks: [], not_committed: null },
  children: { available: true, total: 1 },
  stats: {
    available: true,
    model: "claude-opus-4-8",
    stats: {
      input_tokens: 1000, output_tokens: 2000, cache_read_tokens: 3000, cache_write_tokens: 400, cache_hit_ratio: 0.75,
      turns: 2, tool_calls: 5, assistant_messages: 4, usage_conflicts: 0, usage_schema_unknown: 0,
      context_tokens: 50000, cost_usd: null,
      time: { wall_secs: 600, active_secs: 300, turns_timed: 2, turn_mean_secs: 30, turn_median_secs: 30, turn_max_secs: 40, turn_source: "timestamps", wait_secs: null, wait_gaps: 0, wait_median_secs: null, wait_gaps_excluded: 0, pending_ask: false, clock_regressions: 0 },
    },
  },
};

async function admit(page: Page): Promise<void> {
  await page.route("**/api/identity", async (route) => {
    const res = await route.fetch();
    const j = (await res.json()) as Record<string, unknown>;
    await route.fulfill({ response: res, json: { ...j, terminal_admitted: true } });
  });
}

test.describe("RAILNOW Now tab", () => {
  test("is the first tab; a blocked row shows needs-you and badge 1; unblocking removes both", async ({ page }) => {
    await installFakeTerminal(page, { total: 300, seedLines: 100 });
    await admit(page);
    const state = { blocked: true, livePaths: [] as string[] };
    await page.route("**/api/sessions/live-status", (route) =>
      route.fulfill({ status: 200, contentType: "application/json", body: JSON.stringify({ sessions: [liveRow(state.blocked)] }) }),
    );
    await page.route(new RegExp(`/api/sessions/${SID}/card$`), (route) =>
      route.fulfill({ status: 200, contentType: "application/json", body: JSON.stringify(card) }),
    );
    page.on("request", (r) => {
      if (/\/api\/sessions\/[^/]+\/live/.test(r.url())) state.livePaths.push(r.url());
    });

    await page.goto(`${BASE}/term/${TERM_ID}`);
    const tab = page.locator('[data-testid="term-tab-now"]');
    await expect(page.getByRole("tab").first()).toHaveAttribute("data-testid", "term-tab-now");
    await expect(tab.locator(".kb-termshell__badge")).toHaveText("1", { timeout: 20_000 });
    await tab.click();

    await expect(page.locator('[data-testid="now-sec-needs"]')).toBeVisible();
    await expect(page.locator('[data-testid="now-needs-text"]')).toHaveText("synthetic permission prompt");
    await expect(page.locator('[data-testid="now-last-tool"]')).toContainText("Edit");
    await expect(page.locator('[data-testid="now-answer"]')).toHaveText("synthetic final answer");
    await expect(page.locator('[data-stat="context"]')).toContainText("41%");
    await expect(page.locator('[data-stat="cost"]')).toHaveCount(0);
    // PQUI filled the seam: the queue section is visible (unbound caption here).
    await expect(page.locator('[data-testid="now-queue"]')).toBeVisible();

    state.blocked = false;
    await expect(tab.locator(".kb-termshell__badge")).toHaveCount(0, { timeout: 25_000 });
    await expect(page.locator('[data-testid="now-sec-needs"]')).toHaveCount(0);
    expect(state.livePaths).toEqual([]);
  });
});
