import { expect, test } from "@playwright/test";
import { KNOWN_FILE, KNOWN_SYMBOL } from "./fixture-repo";
import { BASE, REPO_NAME } from "./helpers";

/// V3.1-H3b — Code Vision lenses: usage chips on declarations; pref hides them.

async function openKnownFile(page: import("@playwright/test").Page) {
  await page.goto(`${BASE}/r/${REPO_NAME}`);
  await page.locator(".kbc-tree__row", { hasText: KNOWN_FILE }).click();
  await expect(page.locator(".kbc-codeview")).toContainText(KNOWN_SYMBOL, { timeout: 10_000 });
}

test.describe("Code Vision lenses (V3.1-H3b)", () => {
  test("usage chips appear on the fixture's declarations", async ({ page }) => {
    await openKnownFile(page);
    await page.locator(".kbc-codeview").click();

    const chip = page.locator("[data-kbc-lens]").first();
    await expect(chip).toBeVisible({ timeout: 15_000 });
    await expect(page.locator("[data-kbc-lens-usages]").first()).toBeVisible();
    // pain is always null this wave — never a pain chip attribute/class.
    await expect(page.locator("[data-kbc-lens-pain]")).toHaveCount(0);
  });

  test("toggling the Lenses pref hides chips", async ({ page }) => {
    await openKnownFile(page);
    await page.locator(".kbc-codeview").click();
    await expect(page.locator("[data-kbc-lens]").first()).toBeVisible({ timeout: 15_000 });

    const toggle = page.locator("[data-kbc-lenses-toggle]");
    await expect(toggle).toBeVisible();
    await toggle.click();
    await expect(toggle).toHaveAttribute("aria-pressed", "false");
    await expect(page.locator("[data-kbc-lens]")).toHaveCount(0, { timeout: 5_000 });
  });
});
