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
