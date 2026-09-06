import { expect, test } from "@playwright/test";
import { KNOWN_FILE, KNOWN_SYMBOL } from "./fixture-repo";
import { BASE, REPO_NAME } from "./helpers";

/// V3.1-H3b — impact panel (`gi`): opens, buckets render with class badges,
/// Tests bucket always present (last).

async function openKnownFile(page: import("@playwright/test").Page) {
  await page.goto(`${BASE}/r/${REPO_NAME}`);
  await page.locator(".kbc-tree__row", { hasText: KNOWN_FILE }).click();
  await expect(page.locator(".kbc-codeview")).toContainText(KNOWN_SYMBOL, { timeout: 10_000 });
}

test.describe("impact panel (V3.1-H3b)", () => {
  test("gi opens impact panel with buckets + class badges + tests section", async ({
    page,
  }) => {
    await openKnownFile(page);
    await page.locator(".kbc-codeview").getByText(KNOWN_SYMBOL, { exact: true }).first().click();

    await page.keyboard.press("g");
    await page.keyboard.press("i");

    const panel = page.locator("[data-kbc-impact]");
    await expect(panel).toBeVisible({ timeout: 15_000 });

    // Honesty note from the server is visible in the header area.
    await expect(panel.locator("[data-kbc-impact-note]")).toBeVisible({ timeout: 10_000 });

    // All five buckets render as section headers; Tests last.
    await expect(panel.locator('[data-kbc-impact-section="direct_exact"]')).toBeVisible();
    await expect(panel.locator('[data-kbc-impact-section="direct_likely"]')).toBeVisible();
    await expect(panel.locator('[data-kbc-impact-section="transitive"]')).toBeVisible();
    await expect(panel.locator('[data-kbc-impact-section="imports"]')).toBeVisible();
    await expect(panel.locator('[data-kbc-impact-section="tests"]')).toBeVisible();

    // At least one class badge on a navigable row (when the fixture has signal).
    const rows = panel.locator("[data-kbc-impact-row]");
    if ((await rows.count()) > 0) {
      await expect(panel.locator("[data-kbc-hier-class]").first()).toBeVisible();
    }

    await page.keyboard.press("Escape");
    await expect(panel).toHaveCount(0);
  });
});
