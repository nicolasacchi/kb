import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";
import { expect, test, type Page } from "@playwright/test";
import { BASE, REPO_DIR, REPO_NAME } from "./helpers";
import { KNOWN_FILE } from "./fixture-repo";

/// OL F10 -- the chrome budget: how many pixels of fixed/sticky page chrome sit
/// on the top edge of Home and of the reader. ui/chrome-budget.json holds the
/// frozen numbers; the C units lower them in the PR that removes chrome.
const budgets = JSON.parse(
  readFileSync(fileURLToPath(new URL("../../../ui/chrome-budget.json", import.meta.url)), "utf8"),
) as { "kb-code": { home: number; reader: number } };

test.use({ viewport: { width: 1280, height: 720 }, reducedMotion: "reduce" });

/// Total height of the page chrome pinned to the top edge: the union (de-duplicated
/// by vertical overlap) of the rects of every visible `position: fixed|sticky`
/// element whose top is at or above the viewport top and which still intersects
/// that edge. Elements taller than half the viewport are layout columns or
/// overlays (a fixed side rail, a scrim), not a bar, and are skipped.
async function pinnedTopChrome(page: Page): Promise<number> {
  return page.evaluate(() => {
    const vh = window.innerHeight;
    const spans: Array<[number, number]> = [];
    for (const el of Array.from(document.querySelectorAll<HTMLElement>("body *"))) {
      const cs = getComputedStyle(el);
      if (cs.position !== "fixed" && cs.position !== "sticky") continue;
      if (cs.display === "none" || cs.visibility === "hidden") continue;
      const r = el.getBoundingClientRect();
      if (r.width < 1 || r.height < 1) continue;
      if (r.top > 0.5 || r.bottom <= 0) continue;
      if (r.height > vh / 2) continue;
      spans.push([Math.max(0, r.top), r.bottom]);
    }
    spans.sort((a, b) => a[0] - b[0]);
    let total = 0;
    let end = -Infinity;
    let start = 0;
    for (const [s, e] of spans) {
      if (s > end) {
        if (end > -Infinity) total += end - start;
        start = s;
        end = e;
      } else if (e > end) end = e;
    }
    if (end > -Infinity) total += end - start;
    return Math.round(total);
  });
}

/// Two-way: above budget is a regression; more than 4px below it means the
/// budget is stale and must be lowered in the same PR.
function assertBudget(app: string, route: string, measured: number, budget: number): void {
  console.log(`chrome-budget: ${app}.${route} measured=${measured} budget=${budget}`);
  expect(measured, `${app}.${route} chrome grew past ui/chrome-budget.json`).toBeLessThanOrEqual(budget);
  expect(measured, `${app}.${route} is under budget by >4px: lower ui/chrome-budget.json`).toBeGreaterThanOrEqual(
    budget - 4,
  );
}

test.describe("kb-code top chrome (OL F10)", () => {
  test.beforeEach(() => {
    test.skip(!REPO_DIR, "KB_CODE_E2E_REPO_DIR not set — global-setup didn't run");
  });

  test("home stays within ui/chrome-budget.json", async ({ page }) => {
    await page.goto(`${BASE}/`);
    await expect(page.locator(`[data-kbc-home-card="${REPO_NAME}"]`)).toBeVisible({ timeout: 15_000 });
    await page.evaluate(() => document.fonts.ready);
    assertBudget("kb-code", "home", await pinnedTopChrome(page), budgets["kb-code"].home);
  });

  test("reader stays within ui/chrome-budget.json", async ({ page }) => {
    await page.goto(`${BASE}/r/${REPO_NAME}/${KNOWN_FILE}?desk=read`);
    await expect(page.locator(".kbc-codeview")).toBeVisible({ timeout: 15_000 });
    await page.evaluate(() => document.fonts.ready);
    assertBudget("kb-code", "reader", await pinnedTopChrome(page), budgets["kb-code"].reader);
  });
});
