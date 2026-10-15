import { test, expect } from "@playwright/test";
import { PORT } from "./helpers";

// OL L5 — the Index view (?view=list): sticky sortable header + column chooser.
const URL = `http://127.0.0.1:${PORT}/?kb=canon&view=list`;

test.describe("Index view", () => {
  test("shows the header with the default columns", async ({ page }) => {
    await page.goto(URL);
    await expect(page.getByTestId("index-head")).toBeVisible();
    await expect(page.locator('[data-testid="index-head"] [data-col="title"]')).toBeVisible();
    await expect(page.locator('[data-testid="index-head"] [data-col="words"]')).toBeVisible();
    await expect(page.locator('[data-testid="index-head"] [data-col="category"]')).toHaveCount(0);
  });

  test("a column toggle persists across reload", async ({ page }) => {
    await page.goto(URL);
    await page.getByTestId("index-columns").click();
    await page.getByTestId("index-col-category").check();
    await expect(page.locator('[data-testid="index-head"] [data-col="category"]')).toBeVisible();
    await page.reload();
    await expect(page.locator('[data-testid="index-head"] [data-col="category"]')).toBeVisible();
    // restore the default so later specs on this daemon see the default Index
    await page.getByTestId("index-columns").click();
    await page.getByTestId("index-col-category").uncheck();
    await expect(page.locator('[data-testid="index-head"] [data-col="category"]')).toHaveCount(0);
  });

  test("clicking a header drives ?sort= and ?dir=", async ({ page }) => {
    await page.goto(URL);
    await page.getByTestId("index-sort-words").click();
    await expect(page).toHaveURL(/sort=words/);
    await expect(page.locator('[data-col="words"]')).toHaveAttribute("aria-sort", "descending");
    await page.getByTestId("index-sort-words").click();
    await expect(page).toHaveURL(/dir=asc/);
    await expect(page.locator('[data-col="words"]')).toHaveAttribute("aria-sort", "ascending");
    await page.getByTestId("index-sort-title").click();
    await expect(page).toHaveURL(/sort=title/);
  });
});
