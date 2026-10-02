import { expect, test } from "@playwright/test";
import { KNOWN_FILE } from "./fixture-repo";
import { BASE, REPO_DIR, REPO_NAME } from "./helpers";

/// v0.44 K4 (A9-1) — a stale or foreign `?review=<id>` must not become a
/// "current review" that renders a false "no comments yet": the reader
/// probes the review, clears the marker, strips the param and says so.

test.describe("a stale ?review= marker is cleared, not trusted (A9-1)", () => {
  test("a review id that does not exist clears the marker and strips ?review=", async ({ page }) => {
    test.skip(!REPO_DIR, "KB_CODE_E2E_REPO_DIR not set — global-setup didn't run");

    await page.goto(`${BASE}/r/${REPO_NAME}/${KNOWN_FILE}?review=987654`);

    // The marker never survives: the chip disappears and the param is gone.
    await expect(page).not.toHaveURL(/review=987654/, { timeout: 15_000 });
    await expect(page.locator('[data-kbc-current-review-chip="bar"]')).toHaveCount(0);
    await expect(page.locator('[data-kbc-itab="review"]')).toHaveCount(0);
    // And the operator is told why (the toast host renders the message).
    await expect(page.getByText("doesn't exist in this repo")).toBeVisible({ timeout: 10_000 });
  });

  test("a non-integer ?review= is never an address", async ({ page }) => {
    test.skip(!REPO_DIR, "KB_CODE_E2E_REPO_DIR not set — global-setup didn't run");

    await page.goto(`${BASE}/r/${REPO_NAME}/${KNOWN_FILE}?review=1e3`);
    await expect(page.locator('[data-kbc-current-review-chip="bar"]')).toHaveCount(0);
    await expect(page.locator('[data-kbc-itab="review"]')).toHaveCount(0);
  });
});
