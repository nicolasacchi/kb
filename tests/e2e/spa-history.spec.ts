import { test, expect } from "@playwright/test";
import { PORT } from "./helpers";

// v0.6+ H5 — gallery history view + open/scroll/search recording.
// The fixture daemon is shared with the other specs; tests should
// touch a unique artifact per case so they don't entangle with other
// specs' history rows.
test.describe("gallery history view", () => {
  function base(): string {
    return `http://127.0.0.1:${PORT}`;
  }

  test("history view toggle is visible alongside grid/list/atlas", async ({ page }) => {
    await page.goto(`${base()}/`);
    await expect(page.getByRole("tab", { name: "History" })).toBeVisible();
  });

  test("opening an artifact then visiting history shows an open row", async ({
    page,
  }) => {
    // Land on the gallery so we can pick a specific artifact.
    await page.goto(`${base()}/?view=list`);
    const firstRow = page.getByRole("link", { name: /Visualizing the Borrow Checker/ }).first();
    await expect(firstRow).toBeVisible();
    await firstRow.click();

    // detail.tsx fires POST /history/open on mount.
    await expect(page.locator(".detail__frame")).toBeVisible();

    // Switch to history view.
    await page.goto(`${base()}/?view=history`);
    await expect(page.getByRole("tab", { name: "History" })).toHaveAttribute(
      "aria-selected",
      "true",
    );

    // The artifact title shows up as an open row.
    await expect(
      page.locator(".history__row--open").getByText(
        /Visualizing the Borrow Checker/,
      ),
    ).toBeVisible({ timeout: 10_000 });
  });

  test("search via cmdk + Enter records a search history row", async ({
    page,
  }) => {
    await page.goto(`${base()}/`);
    await page.getByRole("button", { name: /open search/i }).click();
    const dialog = page.getByRole("dialog", { name: "search" });
    await expect(dialog).toBeVisible();

    // Tab into keyword mode (no embedder in fixture).
    await page.keyboard.press("Tab");

    await page.getByRole("textbox", { name: "search query" }).fill("borrow");
    await expect(
      dialog.getByRole("option", { name: /Borrow Checker/ }),
    ).toBeVisible({ timeout: 5_000 });
    await page.keyboard.press("Enter");

    // Detail view loads.
    await expect(page.locator(".detail__frame")).toBeVisible();

    // History view shows the search. Use .first() because earlier
    // specs in the suite (spa-cmdk) also search "borrow" — the daemon
    // accumulates rows across the run; we just care that the recording
    // works (at least one row with our query appears).
    await page.goto(`${base()}/?view=history`);
    await page.getByRole("tab", { name: "Searches" }).click();
    await expect(
      page.locator(".history__row--search").getByText("borrow").first(),
    ).toBeVisible({ timeout: 10_000 });
  });

  test("filter chips switch between kinds", async ({ page }) => {
    await page.goto(`${base()}/?view=history`);
    await expect(page.getByRole("tab", { name: "All" })).toHaveAttribute(
      "aria-selected",
      "true",
    );
    await page.getByRole("tab", { name: "Opens" }).click();
    await expect(page.getByRole("tab", { name: "Opens" })).toHaveAttribute(
      "aria-selected",
      "true",
    );
    await page.getByRole("tab", { name: "Searches" }).click();
    await expect(page.getByRole("tab", { name: "Searches" })).toHaveAttribute(
      "aria-selected",
      "true",
    );
  });
});
