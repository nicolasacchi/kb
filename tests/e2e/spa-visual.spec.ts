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

// OL F2 -- the theme swap (hand-written palette -> generated kb-classic) must
// not move a pixel beyond rounding. Tighter than the config-wide 0.02.
const MAX_DIFF = 0.01;

const MASKS = [
  ".kb-card__age",
  ".kb-card__new",
  "time",
  // Daemon-status indicator in the Spine: idle/active/warn follows whatever
  // the earlier specs left behind.
  ".kb-statusind",
  // Bottom-right StatusPill: mounted only while it has something to show, so
  // it comes and goes between runs.
  ".pill-container",
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

// The inspector's Reading / Read-by-you blocks show this browser's reading
// progress, which earlier specs leave in varying states; the "stopped at"
// line even changes how many lines the column takes. Hide them (and their
// headings) outright rather than mask, so nothing below them shifts.
const HIDE_READING_STATE = `
  .kb-pinsp__reading, .kb-pinsp__progress, .kb-pinsp__readby,
  .kb-pinsp__body h4:has(+ .kb-pinsp__reading) { display: none !important; }
`;

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
        maxDiffPixelRatio: MAX_DIFF,
        mask: [...MASKS.map((s) => page.locator(s)), ...textMasks(page)],
      });
    });

    // OL L3 -- the unfiltered landing: the Front Page band above the shelf.
    // The edition's content moves with the clock and with whatever earlier
    // specs captured into the kb, so the stories, edition line and cards are
    // masked; what the golden pins is the masthead and the band's frame.
    test("front page", async ({ page }) => {
      await setTheme(page, theme);
      await page.goto(`${BASE}/?kb=canon`);
      await expect(page.locator('[data-testid="frontpage"]')).toBeVisible();
      await expect(page.locator(".kb-card").first()).toBeVisible();
      await settle(page);
      await expect(page).toHaveScreenshot(`frontpage-${theme}.png`, {
        maxDiffPixelRatio: MAX_DIFF,
        mask: [
          ...MASKS.map((s) => page.locator(s)),
          ...textMasks(page),
          page.locator(".kb-fp__dateline"),
          page.locator(".kb-fp__main"),
          page.locator(".kb-fp__coverage"),
          page.locator(".kb-card"),
        ],
      });
    });

    test("artifact reader", async ({ page }) => {
      await setTheme(page, theme);
      await page.goto(`${BASE}/a/canon/kitchen-sink.html`);
      await expect(page.locator("iframe").first()).toBeVisible();
      await page.addStyleTag({ content: HIDE_READING_STATE });
      await settle(page);
      await expect(page).toHaveScreenshot(`reader-${theme}.png`, {
        maxDiffPixelRatio: MAX_DIFF,
        mask: [...MASKS.map((s) => page.locator(s)), ...textMasks(page), page.locator("iframe")],
      });
    });
  });
}
