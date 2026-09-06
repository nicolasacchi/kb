import { expect, test } from "@playwright/test";
import { KNOWN_SYMBOL } from "./fixture-repo";
import { BASE } from "./helpers";

test.describe("/search page", () => {
  test("renders the fixed lane sections for a query carried in the URL", async ({ page }) => {
    await page.goto(`${BASE}/search?q=${encodeURIComponent(KNOWN_SYMBOL)}`);
    await expect(page.locator("[data-kbc-lane]")).toHaveCount(6, { timeout: 10_000 });
    await expect(page.locator('[data-kbc-lane="symbols"]')).toContainText(KNOWN_SYMBOL);
  });

  test("clicking the symbols prefix chip narrows the query to one lane and updates the input", async ({
    page,
  }) => {
    await page.goto(`${BASE}/search`);
    const input = page.getByRole("textbox", { name: "search query" });
    await input.fill(KNOWN_SYMBOL);
    await page.getByRole("button", { name: /symbols prefix/i }).click();

    await expect(input).toHaveValue(`@${KNOWN_SYMBOL}`);
    await expect(page.locator("[data-kbc-lane]")).toHaveCount(1, { timeout: 10_000 });
    await expect(page.locator('[data-kbc-lane="symbols"]')).toContainText(KNOWN_SYMBOL);

    // The debounced URL sync round-trips q= so the narrowed search stays
    // shareable.
    await expect(page).toHaveURL(new RegExp(`q=%40${KNOWN_SYMBOL}`));
  });
});
