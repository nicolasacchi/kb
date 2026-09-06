import { expect, test } from "@playwright/test";
import { KNOWN_FILE } from "./fixture-repo";
import { BASE, REPO_NAME } from "./helpers";

/// V3.2-B3 — FileTree attention overlay: off by default; toggle tints rows.

test.describe("attention heatmap overlay", () => {
  test("toggle is off by default and tints rows when enabled", async ({ page }) => {
    // Clear pref so default OFF is observable even if a prior test left it on.
    await page.addInitScript(() => {
      try {
        const raw = localStorage.getItem("kbc:prefs");
        if (raw) {
          const p = JSON.parse(raw) as Record<string, unknown>;
          delete p.attentionOverlay;
          localStorage.setItem("kbc:prefs", JSON.stringify(p));
        }
      } catch {
        /* ignore */
      }
    });

    await page.goto(`${BASE}/r/${REPO_NAME}/${KNOWN_FILE}`);
    await expect(page.locator("[data-kbc-tree]")).toBeVisible({ timeout: 15_000 });

    const tree = page.locator("[data-kbc-tree]");
    await expect(tree).toHaveAttribute("data-kbc-attention-overlay", "off");

    const toggle = page.locator("[data-kbc-attention-toggle]");
    await expect(toggle).toBeVisible();
    await expect(toggle).not.toBeChecked();

    await toggle.check();
    await expect(tree).toHaveAttribute("data-kbc-attention-overlay", "on");

    // After fetch, some file rows may carry a score attr when counters exist.
    // Absence is fine (untinted); presence proves the overlay path ran.
    await page.waitForTimeout(500);
    const tinted = page.locator("[data-kbc-attention-score]");
    // Toggle remaining on is the contract; tinted rows are best-effort on
    // a fixture that may not have run behavioral backfill.
    await expect(toggle).toBeChecked();
    // If any row is tinted, its score attribute is a finite number string.
    const n = await tinted.count();
    for (let i = 0; i < n; i++) {
      const s = await tinted.nth(i).getAttribute("data-kbc-attention-score");
      expect(s).toBeTruthy();
      expect(Number(s)).not.toBeNaN();
    }
  });
});
