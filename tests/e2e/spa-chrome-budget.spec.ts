import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";
import { test, expect, type Page } from "@playwright/test";
import { BASE } from "./helpers";

// OL F10 -- the chrome budget: how many pixels of fixed/sticky page chrome sit
// on the top edge of the gallery. ui/chrome-budget.json holds the frozen number;
// the L units lower it in the PR that removes chrome.
const budgets = JSON.parse(
  readFileSync(fileURLToPath(new URL("../../ui/chrome-budget.json", import.meta.url)), "utf8"),
) as { kb: { gallery: number } };

test.use({ viewport: { width: 1440, height: 900 }, reducedMotion: "reduce" });

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

test("kb gallery top chrome stays within ui/chrome-budget.json", async ({ page }) => {
  await page.goto(`${BASE}/?kb=canon&folder=pm&sort=title&dir=asc`);
  await expect(page.locator(".kb-card").first()).toBeVisible();
  await page.evaluate(() => document.fonts.ready);
  await page.waitForTimeout(500);
  assertBudget("kb", "gallery", await pinnedTopChrome(page), budgets.kb.gallery);
});
