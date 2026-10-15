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

// Clock- and visit-dependent text (timestamps in the passport, "active 4s",
// visit counts) is masked by content so the goldens only move with styling.
function textMasks(page: Page) {
  return [
    page.getByText(/\d{4}-\d{2}-\d{2}/),
    page.getByText(/active \d+\w+ .* visit/),
    page.getByText(/\b\d+ visits?\b/),
  ];
}

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
      // The static pm/ fixture folder, title-sorted: other specs add timestamped
      // capture docs to the kb root, which would make the full gallery drift.
      await page.goto(`${BASE}/?kb=canon&folder=pm&sort=title&dir=asc`);
      await expect(page.locator(".kb-card").first()).toBeVisible();
      await settle(page);
      await expect(page).toHaveScreenshot(`gallery-${theme}.png`, {
        mask: [...MASKS.map((s) => page.locator(s)), ...textMasks(page)],
      });
    });

    test("artifact reader", async ({ page }) => {
      await setTheme(page, theme);
      await page.goto(`${BASE}/a/canon/kitchen-sink.html`);
      await expect(page.locator("iframe").first()).toBeVisible();
      await settle(page);
      await expect(page).toHaveScreenshot(`reader-${theme}.png`, {
        mask: [...MASKS.map((s) => page.locator(s)), ...textMasks(page), page.locator("iframe")],
      });
    });
  });
}
