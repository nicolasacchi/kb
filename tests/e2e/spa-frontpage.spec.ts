import { test, expect } from "@playwright/test";
import { BASE } from "./helpers";

// OL L3 -- the Front Page band opens the unfiltered Library. Cards stay below
// it (so every spec that expects cards at `/?kb=...` keeps working); any
// filter hides the band; Back restores it; Collapse is remembered per kb.

const FP = '[data-testid="frontpage"]';

test.describe("front page band", () => {
  test("masthead shows with cards below it", async ({ page }) => {
    await page.goto(`${BASE}/?kb=canon`);
    const band = page.locator(FP);
    await expect(band).toBeVisible();
    await expect(band.getByRole("heading", { name: "canon" })).toBeVisible();
    await expect(band.locator("time")).toBeVisible();
    await expect(band.getByTestId("frontpage-edition")).toContainText(/artifacts?/);
    await expect(page.locator(".kb-card").first()).toBeVisible();
    // The band sits above the shelf, not beside or beneath it.
    const bandBox = await band.boundingBox();
    const cardBox = await page.locator(".kb-card").first().boundingBox();
    expect(bandBox && cardBox && bandBox.y < cardBox.y).toBe(true);
  });

  test("a filter hides the band and Back restores it", async ({ page }) => {
    await page.goto(`${BASE}/?kb=canon`);
    await expect(page.locator(FP)).toBeVisible();
    await page.goto(`${BASE}/?kb=canon&folder=pm`);
    await expect(page.locator(".kb-card").first()).toBeVisible();
    await expect(page.locator(FP)).toHaveCount(0);
    await page.goBack();
    await expect(page.locator(FP)).toBeVisible();
  });

  test("a non-default sort hides the band", async ({ page }) => {
    await page.goto(`${BASE}/?kb=canon&sort=title&dir=asc`);
    await expect(page.locator(".kb-card").first()).toBeVisible();
    await expect(page.locator(FP)).toHaveCount(0);
  });

  test("Collapse persists across reload", async ({ page }) => {
    await page.goto(`${BASE}/?kb=canon`);
    const band = page.locator(FP);
    await expect(band).toBeVisible();
    await band.getByTestId("frontpage-collapse").click();
    await expect(band.locator(".kb-fp__page")).toHaveCount(0);
    await page.reload();
    await expect(band).toBeVisible();
    await expect(band.locator(".kb-fp__page")).toHaveCount(0);
    await band.getByTestId("frontpage-collapse").click();
    await expect(band.locator(".kb-fp__page")).toBeVisible();
  });
});

// OL IDEA5 -- "Since you left": the project-axis strip. A fresh browser
// context has no visit record, so no other spec here ever sees it.
const VISIT_CANON = "kb:frontpage:visit:canon";
const STRIP = '[data-testid="since-you-left"]';

test.describe("front page since-you-left strip", () => {
  test("since strip lists a slate post made after the last visit", async ({ page, request }) => {
    const nowS = Math.floor(Date.now() / 1000);
    await page.addInitScript(
      ([key, last]) => {
        if (!localStorage.getItem(key as string)) {
          localStorage.setItem(key as string, JSON.stringify({ last, prev: null }));
        }
      },
      [VISIT_CANON, nowS - 3600],
    );
    const slug = `e2e-since-${Math.random().toString(36).slice(2, 8)}`;
    const line = `since strip handoff ${slug}`;
    const r = await request.post(`${BASE}/api/slates/${slug}/posts`, {
      data: {
        kind: "hand",
        line,
        subject: `e2e/${slug}`,
        prov: { harness: "claude", origin: "agent", session_id: `e2e-${slug}` },
      },
      headers: { "Content-Type": "application/json" },
    });
    expect(r.status()).toBe(201);

    await page.goto(`${BASE}/?kb=canon`);
    const strip = page.locator(STRIP);
    await expect(strip).toBeVisible();
    await expect(strip).toContainText(slug);
    await expect(strip).toContainText(line);
    await expect(strip.getByRole("link", { name: "Corpus activity since then" })).toBeVisible();

    // No horizontal overflow at a phone width.
    await page.setViewportSize({ width: 390, height: 844 });
    const fits = await strip.evaluate((el) => el.scrollWidth <= el.clientWidth);
    expect(fits).toBe(true);
  });

  test("no strip on a first visit", async ({ page }) => {
    await page.goto(`${BASE}/?kb=canon`);
    await expect(page.locator(FP)).toBeVisible();
    await expect(page.locator(STRIP)).toHaveCount(0);
  });
});
