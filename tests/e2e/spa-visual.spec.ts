import { test, expect, type Page } from "@playwright/test";
import { BASE } from "./helpers";

// OL F3 -- visual baselines for the two surfaces most affected by the type /
// spacing / radius token sweep: the gallery and one artifact reader page, at
// 1440x900, dark and light. Goldens are generated on CI (the reference
// renderer) with `workflow_dispatch update_snapshots=true`; see
// playwright.config.ts for the screenshot path template and tolerance.
//
// The corpus is the fixed canon fixture set. Anything that depends on the
// clock (card ages, "new" dots, daemon status, relative times) is masked so the
// goldens only move when styling moves.

test.use({ viewport: { width: 1440, height: 900 }, reducedMotion: "reduce" });

const MASKS = [
  ".kb-card__age",
  ".kb-card__new",
  "time",
  "[class*='age']",
  "[class*='mtime']",
  "[aria-label*='daemon status' i]",
];

async function setTheme(page: Page, theme: "dark" | "light") {
  await page.addInitScript((t) => {
    localStorage.setItem("kb:prefs", JSON.stringify({ theme: t }));
  }, theme);
}

async function settle(page: Page) {
  // Not "networkidle": the SPA keeps an SSE stream open, so the network never
  // idles. Fonts loaded + two animation frames + a short beat is the settle.
  await page.evaluate(() => document.fonts.ready);
  await page.evaluate(
    () => new Promise<void>((r) => requestAnimationFrame(() => requestAnimationFrame(() => r()))),
  );
  await page.waitForTimeout(500);
}

for (const theme of ["dark", "light"] as const) {
  test.describe(`spa visual (${theme})`, () => {
    test("gallery", async ({ page }) => {
      await setTheme(page, theme);
      await page.goto(`${BASE}/?kb=canon`);
      await expect(page.locator(".kb-card").first()).toBeVisible();
      await settle(page);
      await expect(page).toHaveScreenshot(`gallery-${theme}.png`, {
        mask: MASKS.map((s) => page.locator(s)),
      });
    });

    test("artifact reader", async ({ page }) => {
      await setTheme(page, theme);
      await page.goto(`${BASE}/a/canon/kitchen-sink.html`);
      await expect(page.locator("iframe").first()).toBeVisible();
      await settle(page);
      await expect(page).toHaveScreenshot(`reader-${theme}.png`, {
        mask: [...MASKS.map((s) => page.locator(s)), page.locator("iframe")],
      });
    });
  });
}
