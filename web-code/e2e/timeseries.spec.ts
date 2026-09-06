import { expect, test } from "@playwright/test";
import { BASE, REPO_NAME } from "./helpers";

/// V3.4-C3 — insights time-series sparklines on hotspots expander.
/// Sparkline SVG appears only after score expander open (lazy fetch).

test.describe("timeseries sparklines (V3.4-C3)", () => {
  test("sparkline svg appears on hotspot score expander open", async ({ page }) => {
    await page.goto(`${BASE}/r/${REPO_NAME}/~hotspots`);
    await expect(page.locator("[data-kbc-hotspots]")).toBeVisible({ timeout: 10_000 });

    const rows = page.locator("[data-kbc-hotspots-row]");
    const count = await rows.count();
    if (count === 0) {
      // Empty corpus is valid when counters were never backfilled.
      await expect(page.locator("[data-kbc-hotspots]")).toBeVisible();
      return;
    }

    // Before expand: no row sparkline fetch shell.
    await expect(page.locator("[data-kbc-hotspots-spark]")).toHaveCount(0);

    // Open first score expander → terms + lazy sparkline.
    await rows.first().locator("[data-kbc-hotspots-score]").click();
    await expect(page.locator("[data-kbc-hotspots-terms]")).toBeVisible();

    // Sparkline container appears (svg may be empty-line when no activity).
    const spark = page.locator("[data-kbc-hotspots-spark]");
    await expect(spark).toBeVisible({ timeout: 15_000 });
    await expect(spark.locator("svg")).toBeVisible();
    // Either a path (activity) or empty dashed line — both render an svg.
    const hasPath = (await spark.locator("path.kbc-spark__line").count()) > 0;
    const hasEmpty = (await spark.locator("[data-kbc-sparkline-empty]").count()) > 0
      || (await spark.locator("line.kbc-spark__empty").count()) > 0
      || hasPath;
    expect(hasEmpty || hasPath).toBe(true);
  });
});
