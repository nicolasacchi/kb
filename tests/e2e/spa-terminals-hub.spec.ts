import { test, expect, type Page } from "@playwright/test";
import { BASE } from "./helpers";
import { installFakeTerminal, TERM_ID } from "./fixtures/fake-tap.mjs";

// HUBDOOR — the Terminals door (sixth Spine door, admitted callers only), the
// /terminals hub list and the /term/:id redirect. Transport is the fake tap;
// the fixture's identity is terminal-admitted, and the term list is re-routed
// here to carry a second, blocked terminal. CHROMIUM desktop only.

const OTHER_ID = "t-e2e-2";

function row(id: string, o: Record<string, unknown> = {}) {
  return {
    id,
    harness: "omp",
    cwd: "/synthetic",
    started_unix: 1760000000,
    resume_sid: null,
    pane_pid: 1,
    cols: 120,
    rows: 40,
    bracket_paste_flag: false,
    pane_dead: false,
    exit_code: null,
    grid_owner: null,
    conf_version: 1,
    anchor_pid: null,
    stale: false,
    session_id: null,
    state: "working",
    blocked: false,
    ...o,
  };
}

async function twoTerms(page: Page, blockedId: string | null): Promise<void> {
  await page.route("**/api/terms", (route) => {
    if (route.request().method() !== "GET") return route.continue();
    return route.fulfill({
      status: 200,
      contentType: "application/json",
      body: JSON.stringify({
        terms: [row(TERM_ID, { blocked: blockedId === TERM_ID }), row(OTHER_ID, { blocked: blockedId === OTHER_ID })],
      }),
    });
  });
}

async function notAdmitted(page: Page): Promise<void> {
  await page.route("**/api/identity", async (route) => {
    const res = await route.fetch();
    const j = (await res.json()) as Record<string, unknown>;
    await route.fulfill({ response: res, json: { ...j, terminal_admitted: false } });
  });
}

const DOOR = '[data-testid="header-terminals-tab"]';

test.describe("HUBDOOR Terminals door and hub", () => {
  test("door with a pill for the one blocked terminal; the hub lists it first and rows open /terminals/<id>", async ({ page }) => {
    await installFakeTerminal(page, { total: 300, seedLines: 100 });
    await twoTerms(page, OTHER_ID);
    await page.goto(`${BASE}/`);
    const door = page.locator(DOOR);
    await expect(door).toBeVisible({ timeout: 20_000 });
    await expect(page.locator('[data-testid="door-badge-terminals"]')).toHaveText("1");
    await expect(page.getByRole("link", { name: /Terminals\s*,\s*1 needs attention/ })).toBeVisible();
    await door.click();
    await expect(page).toHaveURL(/\/terminals$/);
    const rows = page.locator('[data-testid="hub-row"]');
    await expect(rows).toHaveCount(2, { timeout: 15_000 });
    await expect(rows.first()).toHaveAttribute("data-term", OTHER_ID);
    await expect(rows.first()).toHaveAttribute("data-tier", "needs");
    await rows.nth(1).click();
    await expect(page).toHaveURL(new RegExp(`/terminals/${TERM_ID}$`));
    await expect(page.locator("[data-kb-term]")).toHaveAttribute("data-kb-term-phase", "live", { timeout: 30_000 });
  });

  test("/term/<id> lands on /terminals/<id> with the viewport live", async ({ page }) => {
    await installFakeTerminal(page, { total: 300, seedLines: 100 });
    await page.goto(`${BASE}/term/${TERM_ID}`);
    await expect(page).toHaveURL(new RegExp(`/terminals/${TERM_ID}$`));
    await expect(page.locator("[data-kb-term]")).toHaveAttribute("data-kb-term-phase", "live", { timeout: 30_000 });
  });

  test("a caller without the terminal tier sees no door and a refused card on /terminals", async ({ page }) => {
    await installFakeTerminal(page, { total: 300, seedLines: 100 });
    await notAdmitted(page);
    await page.goto(`${BASE}/`);
    await expect(page.locator('[data-testid="header-search-tab"]')).toBeVisible({ timeout: 20_000 });
    await expect(page.locator(DOOR)).toHaveCount(0);
    await page.goto(`${BASE}/terminals`);
    await expect(page.getByText("Terminals are not enabled")).toBeVisible({ timeout: 15_000 });
  });

  test("at 1000px the label collapses and the pill stays", async ({ page }) => {
    await page.setViewportSize({ width: 1000, height: 720 });
    await installFakeTerminal(page, { total: 300, seedLines: 100 });
    await twoTerms(page, TERM_ID);
    await page.goto(`${BASE}/`);
    await expect(page.locator('[data-testid="door-badge-terminals"]')).toBeVisible({ timeout: 20_000 });
    const w = await page.locator(`${DOOR} .kb-door__label`).evaluate((el) => el.getBoundingClientRect().width);
    expect(w).toBeLessThanOrEqual(1);
  });

  for (const width of [861, 915, 1000]) {
    test(`six doors fit at ${width}px: no sideways page scroll, bar inside the viewport`, async ({ page }) => {
      await page.setViewportSize({ width, height: 720 });
      await installFakeTerminal(page, { total: 300, seedLines: 100 });
      await twoTerms(page, TERM_ID);
      await page.goto(`${BASE}/`);
      await expect(page.locator(DOOR)).toBeVisible({ timeout: 20_000 });
      const fit = await page.evaluate(() => ({
        inner: window.innerWidth,
        doc: document.documentElement.scrollWidth,
        right: Math.max(...[...document.querySelectorAll(".kb-doors .kb-door")].map((e) => e.getBoundingClientRect().right)),
      }));
      expect(fit.doc, "document scrollWidth").toBeLessThanOrEqual(fit.inner);
      expect(fit.right, "last door right edge").toBeLessThanOrEqual(fit.inner);
    });
  }
});
