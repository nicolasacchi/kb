import { expect, test } from "@playwright/test";
import { SCRUB_BRANCH, SCRUB_FILE, SCRUB_V1, SCRUB_V2, SCRUB_V3 } from "./fixture-repo";
import { BASE, REPO_NAME } from "./helpers";

/// V76-R3d — file-scoped time scrubber. Open a file with 3 commits, step
/// back twice (buffer changes, label is nearest-prior), then step before
/// the floor (miss). Landmark golden is untouched: the strip is a div
/// inside <main>, not a new region.

test.describe("file time scrubber (V76-R3d)", () => {
  test("step back twice then miss at the floor", async ({ page }) => {
    test.skip(!process.env.KB_CODE_E2E_REPO_DIR, "KB_CODE_E2E_REPO_DIR not set — global-setup didn't run");

    await page.goto(`${BASE}/r/${REPO_NAME}/${SCRUB_FILE}?ref=${SCRUB_BRANCH}`);
    await expect(page.locator(".kbc-codeview")).toContainText(SCRUB_V3, { timeout: 10_000 });

    await page.locator(".kbc-codeview").click();
    await page.keyboard.press(" ");
    await page.keyboard.press("H");
    const strip = page.locator("[data-kbc-scrub-strip]");
    await expect(strip).toBeVisible({ timeout: 5_000 });

    await page.keyboard.press("[");
    await page.keyboard.press("H");
    await expect(page.locator(".kbc-codeview")).toContainText(SCRUB_V2, { timeout: 10_000 });
    await expect(page.locator("[data-kbc-scrub-label]")).toContainText("nearest-prior");

    await page.keyboard.press("[");
    await page.keyboard.press("H");
    await expect(page.locator(".kbc-codeview")).toContainText(SCRUB_V1, { timeout: 10_000 });
    await expect(page.locator("[data-kbc-scrub-label]")).toContainText("nearest-prior");

    await page.keyboard.press("[");
    await page.keyboard.press("H");
    await expect(page.locator("[data-kbc-scrub-label]")).toContainText("before the floor");
    await expect(page.locator(".kbc-codeview")).toContainText(SCRUB_V1);
  });
});
