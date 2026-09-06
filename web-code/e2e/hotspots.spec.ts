import { expect, test } from "@playwright/test";
import { BASE, REPO_NAME } from "./helpers";

/// V3.2-B3 — Hotspots page: ranked rows, score expander shows terms,
/// scope filter narrows (when scopes are configured).

test.describe("hotspots page", () => {
  test("renders ranked rows; score expander shows terms", async ({ page }) => {
    await page.goto(`${BASE}/r/${REPO_NAME}/~hotspots`);
    await expect(page.locator("[data-kbc-hotspots]")).toBeVisible({ timeout: 10_000 });
    await expect(page.locator("[data-kbc-hotspots-hint]")).toContainText(/attention/i);

    // After behavioral backfill (or empty corpus), the table may or may not
    // have rows. When rows exist, they are score-sorted and expandable.
    const rows = page.locator("[data-kbc-hotspots-row]");
    const count = await rows.count();
    if (count === 0) {
      // Empty is valid when counters were never backfilled — page still renders.
      await expect(page.locator("[data-kbc-hotspots]")).toBeVisible();
      return;
    }

    // Scores should be non-increasing down the table.
    const scores: number[] = [];
    for (let i = 0; i < Math.min(count, 10); i++) {
      const raw = await rows.nth(i).locator("[data-kbc-hotspots-score]").innerText();
      const n = Number(raw.replace(/[^\d.]/g, ""));
      if (Number.isFinite(n)) scores.push(n);
    }
    for (let i = 1; i < scores.length; i++) {
      expect(scores[i]).toBeLessThanOrEqual(scores[i - 1] + 1e-9);
    }

    // Expand first score → terms list + lazy weekly-churn sparkline (V3.4-C3).
    await rows.first().locator("[data-kbc-hotspots-score]").click();
    const terms = page.locator("[data-kbc-hotspots-terms]");
    await expect(terms).toBeVisible();
    await expect(terms).toContainText("churn_rank");
    await expect(terms).toContainText("complexity_rank");
    // Sparkline mounts only after expand (no eager N fetches).
    await expect(page.locator("[data-kbc-hotspots-spark]").locator("svg")).toBeVisible({
      timeout: 15_000,
    });
  });

  test("scope filter control is present and can narrow", async ({ page }) => {
    await page.goto(`${BASE}/r/${REPO_NAME}/~hotspots`);
    await expect(page.locator("[data-kbc-hotspots]")).toBeVisible({ timeout: 10_000 });

    const scope = page.locator("[data-kbc-hotspots-scope]");
    if ((await scope.count()) === 0) {
      // No [scopes] configured on the e2e daemon — control is correctly absent.
      return;
    }
    const options = scope.locator("option");
    const n = await options.count();
    expect(n).toBeGreaterThan(1);

    // Pick the first non-empty include option if any.
    const values = await options.evaluateAll((els) =>
      els.map((e) => (e as HTMLOptionElement).value).filter((v) => v && !v.startsWith("!")),
    );
    if (values.length === 0) return;

    const before = await page.locator("[data-kbc-hotspots-row]").count();
    await scope.selectOption(values[0]);
    // Wait for query refetch.
    await page.waitForTimeout(400);
    const after = await page.locator("[data-kbc-hotspots-row]").count();
    // Scope can only narrow or keep equal (never invent rows).
    expect(after).toBeLessThanOrEqual(before);
  });
});
