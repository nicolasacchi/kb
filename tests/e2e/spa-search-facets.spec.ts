import { test, expect } from "@playwright/test";
import { PORT } from "./helpers";

// Q-track / FS-track — the faceted search page. The canon e2e kb has no
// embedder, so these tests drive BROWSE mode (empty query + a filter): the
// daemon lists + filters + sorts without embedding. Canon fixtures used:
//   caps=svg  → exactly the one fullscreen-viz artifact (1 <svg>)
//   caps=code → several artifacts with code blocks (≥ 2)

const url = (qs: string) => `http://127.0.0.1:${PORT}/search?${qs}`;
const cards = (page: import("@playwright/test").Page) =>
  page.getByRole("link", { name: /^Open / });

test.describe("faceted search", () => {
  test("browse mode lists artifacts for an empty query + filter", async ({
    page,
  }) => {
    await page.goto(url("kb=canon&caps=code"));
    await expect(cards(page).first()).toBeVisible();
    // Browse header says "artifacts", not "results", and drops the mode.
    await expect(page.locator(".kb-search__meta")).toContainText(/artifacts?/);
    expect(await cards(page).count()).toBeGreaterThanOrEqual(2);
  });

  test("a narrower capability filter returns fewer artifacts", async ({
    page,
  }) => {
    await page.goto(url("kb=canon&caps=code"));
    await expect(cards(page).first()).toBeVisible();
    const codeCount = await cards(page).count();

    await page.goto(url("kb=canon&caps=svg"));
    await expect(cards(page).first()).toBeVisible();
    const svgCount = await cards(page).count();

    // Only fullscreen-viz carries an <svg>; many artifacts carry code.
    expect(svgCount).toBeLessThan(codeCount);
    expect(svgCount).toBeGreaterThanOrEqual(1);
  });

  test("toggling a rail capability writes the URL + re-queries", async ({
    page,
  }) => {
    await page.goto(url("kb=canon"));
    // Empty query + no filter → the search empty-state.
    await expect(page.getByText("Search the knowledge base")).toBeVisible();
    // Expand the Capabilities section, then toggle "svg".
    await page
      .locator(".kb-search-rail__grouph", { hasText: "Capabilities" })
      .click();
    await page
      .getByRole("group", { name: "capabilities" })
      .getByRole("button", { name: "svg" })
      .click();
    await expect(page).toHaveURL(/caps=svg/);
    await expect(cards(page).first()).toBeVisible();
  });

  test("the sort menu round-trips through the URL", async ({ page }) => {
    await page.goto(url("kb=canon&caps=code&sort=title"));
    await expect(cards(page).first()).toBeVisible();
    await expect(page.locator(".sort-control__select")).toHaveValue("title");
    // Switching the sort writes the URL (and drops back to default cleanly).
    await page
      .locator(".sort-control__select")
      .selectOption("modified");
    await expect(page).toHaveURL(/sort=modified/);
  });

  test("a chip removes its own filter", async ({ page }) => {
    await page.goto(url("kb=canon&caps=code"));
    await expect(cards(page).first()).toBeVisible();
    const chip = page.locator(".kb-search-chips__chip", { hasText: "code" });
    await expect(chip).toBeVisible();
    await page.getByRole("button", { name: "remove code" }).click();
    // No query, no filter left → back to the empty state, caps gone.
    await expect(page).not.toHaveURL(/caps=/);
    await expect(page.getByText("Search the knowledge base")).toBeVisible();
  });

  test("clear-all drops every filter but keeps the query", async ({ page }) => {
    await page.goto(url("q=incident&mode=keyword&kb=canon&caps=code&read=unread"));
    // Two active filters → two chips + a clear-all control.
    await expect(
      page.locator(".kb-search-chips__chip", { hasText: "code" }),
    ).toBeVisible();
    await page.getByRole("button", { name: "clear all" }).click();
    await expect(page).not.toHaveURL(/caps=/);
    await expect(page).not.toHaveURL(/read=/);
    // The query itself survives clear-all.
    await expect(page).toHaveURL(/q=incident/);
  });
});
