import { expect, test } from "@playwright/test";
import { KNOWN_FILE, KNOWN_SYMBOL } from "./fixture-repo";
import { BASE, REPO_NAME } from "./helpers";

// V76-R3a — aug-lane/1 Facts surface. global-setup.ts enables rubocop and
// ingests one diagnostic on KNOWN_FILE via the loopback route.

async function openKnownFile(page: import("@playwright/test").Page) {
  await page.goto(`${BASE}/r/${REPO_NAME}/${KNOWN_FILE}`);
  await expect(page.locator(".kbc-codeview")).toContainText(KNOWN_SYMBOL, { timeout: 10_000 });
}

test.describe("aug-lane/1 Facts — rail + diagnostics gutter variant", () => {
  test.use({ viewport: { width: 1280, height: 720 } });

  test("Facts tab lists the ingested rubocop fact; diagnostics gutter shows the lane variant", async ({
    page,
  }) => {
    await openKnownFile(page);

    await page.keyboard.press("Space");
    await page.keyboard.press("R");
    await page.keyboard.press("f");
    await expect(page.locator('[data-kbc-itab="facts"]')).toHaveAttribute("aria-selected", "true");

    const panel = page.locator("[data-kbc-facts-panel]");
    await expect(panel).toBeVisible();
    await expect(panel.locator('[data-kbc-facts-lane="rubocop"]')).toBeVisible();
    await expect(panel).toContainText("Style/FactsLane");
    await expect(panel).toContainText("facts-lane e2e cop");
    await expect(page.locator("[data-kbc-facts-lanes-link]")).toHaveAttribute(
      "href",
      `/r/${REPO_NAME}/~lanes`,
    );

    const laneDot = page.locator(".kbc-diag-dot--src-lane").first();
    await expect(laneDot).toBeVisible();
  });

  test("~lanes lists the enabled rubocop row", async ({ page }) => {
    await page.goto(`${BASE}/r/${REPO_NAME}/~lanes`);
    const row = page.locator('[data-kbc-lanes-row="rubocop"]');
    await expect(row).toBeVisible({ timeout: 10_000 });
    await expect(row).toHaveAttribute("data-kbc-lanes-enabled", "1");
    await expect(row).toContainText("rubocop");
  });
});
