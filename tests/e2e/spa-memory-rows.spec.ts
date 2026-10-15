import { test, expect } from "@playwright/test";
import { PORT } from "./helpers";

// OL R3 — memory rows lead with the memory: a full-title row, one quiet meta
// line, actions behind hover/focus and a ⋯ button, and the decay policy said
// once in the page header rather than on every row.
test.describe("memory rows (OL R3)", () => {
  test("the half-life caption lives in the header, not on each row", async ({ page }) => {
    await page.goto(`http://127.0.0.1:${PORT}/memory`);
    const items = page.getByTestId("memory-item");
    await expect(items.first()).toBeVisible({ timeout: 10_000 });

    await expect(page.getByTestId("memory-halflife")).toContainText("score halves every");
    // No row repeats the caption (the sparkline keeps it in its tooltip only).
    await expect(items.getByText("score halves every")).toHaveCount(0);
  });

  test("a row carries one meta line and a ⋯ button that opens the actions", async ({ page }) => {
    await page.goto(`http://127.0.0.1:${PORT}/memory`);
    const row = page.getByTestId("memory-item").first();
    await expect(row).toBeVisible({ timeout: 10_000 });

    await expect(row.getByTestId("memory-meta")).toContainText("recalls");
    await expect(row.getByTestId("memory-meta")).toContainText("salience");

    const more = row.getByRole("button", { name: "more actions" });
    await expect(more).toHaveAttribute("aria-expanded", "false");
    await more.click();
    await expect(more).toHaveAttribute("aria-expanded", "true");
    await expect(row.getByRole("button", { name: "provenance" })).toBeVisible();
    await expect(row.getByTestId("memory-forget")).toBeVisible();
    await more.press("Escape");
    await expect(more).toHaveAttribute("aria-expanded", "false");
  });

  test("the strip is quiet until the row is hovered", async ({ page }) => {
    await page.goto(`http://127.0.0.1:${PORT}/memory`);
    const row = page.getByTestId("memory-item").first();
    await expect(row).toBeVisible({ timeout: 10_000 });
    const strip = row.locator(".kb-mem__strip");
    await page.mouse.move(0, 0);
    await expect(strip).toHaveCSS("opacity", "0");
    await row.hover();
    await expect(strip).toHaveCSS("opacity", "1");
  });
});
