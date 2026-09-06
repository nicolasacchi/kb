import { expect, test } from "@playwright/test";
import { KNOWN_FILE, KNOWN_SYMBOL } from "./fixture-repo";
import { BASE, REPO_NAME } from "./helpers";

/// V3.1-H3b — ego-graph (`gG`): renders nodes+edges; node click navigates.

async function openKnownFile(page: import("@playwright/test").Page) {
  await page.goto(`${BASE}/r/${REPO_NAME}`);
  await page.locator(".kbc-tree__row", { hasText: KNOWN_FILE }).click();
  await expect(page.locator(".kbc-codeview")).toContainText(KNOWN_SYMBOL, { timeout: 10_000 });
}

test.describe("ego-graph (V3.1-H3b)", () => {
  test("gG renders nodes + edges; node click navigates", async ({ page }) => {
    await openKnownFile(page);
    await page.locator(".kbc-codeview").getByText(KNOWN_SYMBOL, { exact: true }).first().click();

    await page.keyboard.press("g");
    await page.keyboard.press("G");

    const panel = page.locator("[data-kbc-ego]");
    await expect(panel).toBeVisible({ timeout: 15_000 });
    await expect(panel.locator("[data-kbc-ego-svg]")).toBeVisible();

    // Center node always present once loaded.
    const nodes = panel.locator("[data-kbc-ego-node]");
    await expect(nodes.first()).toBeVisible({ timeout: 10_000 });
    const nodeCount = await nodes.count();
    expect(nodeCount).toBeGreaterThanOrEqual(1);

    // Edges may be empty on a sparse fixture, but the SVG marker/path layer exists.
    // Prefer asserting at least one edge when neighborhood is non-trivial.
    const edges = panel.locator("[data-kbc-ego-edge]");
    const edgeCount = await edges.count();
    if (nodeCount > 1) {
      expect(edgeCount).toBeGreaterThanOrEqual(1);
    }

    // Click a non-center node if present → panel closes (navigation).
    const nonCenter = panel.locator("[data-kbc-ego-node][data-kbc-ego-layer]:not([data-kbc-ego-layer='0'])").first();
    if ((await nonCenter.count()) > 0) {
      await nonCenter.click();
      await expect(panel).toHaveCount(0, { timeout: 10_000 });
    } else {
      await page.keyboard.press("Escape");
      await expect(panel).toHaveCount(0);
    }
  });
});
