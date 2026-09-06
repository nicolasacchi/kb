import { expect, test } from "@playwright/test";
import { KNOWN_FILE, KNOWN_SYMBOL } from "./fixture-repo";
import { BASE, REPO_NAME } from "./helpers";

/// V3.N2 — `gO` structure popup: opens, filters, Enter jumps to a symbol.

async function openFixtureFile(page: import("@playwright/test").Page) {
  await page.goto(`${BASE}/r/${REPO_NAME}/${KNOWN_FILE}`);
  await expect(page.locator(".kbc-codeview")).toContainText(KNOWN_SYMBOL, { timeout: 10_000 });
  await page.locator(".kbc-codeview .cm-line").first().click();
}

test.describe("structure popup (gO)", () => {
  test("opens, filters, Enter jumps", async ({ page }) => {
    await openFixtureFile(page);

    await page.keyboard.press("g");
    await page.keyboard.press("O");
    const popup = page.locator("[data-kbc-structure-popup]");
    await expect(popup).toBeVisible();
    await expect(page.locator("[data-kbc-structure-row]").first()).toBeVisible();

    // Filter to the known symbol name.
    await page.locator("[data-kbc-structure-filter]").fill(KNOWN_SYMBOL);
    const rows = page.locator("[data-kbc-structure-row]");
    await expect(rows).toHaveCount(1);
    await expect(rows.first()).toContainText(KNOWN_SYMBOL);

    await page.keyboard.press("Enter");
    await expect(popup).toHaveCount(0);
    // Landing updates the URL `?line=` after the cursor-sync debounce.
    await expect(page).toHaveURL(/[?&]line=\d+/, { timeout: 4_000 });
  });
});
