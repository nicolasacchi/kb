import { expect, test } from "@playwright/test";
import { KNOWN_FILE, KNOWN_SYMBOL } from "./fixture-repo";
import { BASE, REPO_NAME } from "./helpers";

/// V76-R3c — `@ref` on the reader: TopBar chip / Space @ typeahead,
/// table-generated banner, compare `c`. Ctrl-r is a hard-reserved browser
/// chord, so this spec drives Space @ (and the chip) rather than Ctrl-r.

test.describe("reader @ref (V76-R3c)", () => {
  test("Space @ picks HEAD~1, banner is table-generated, c opens compare", async ({ page }) => {
    test.skip(!process.env.KB_CODE_E2E_REPO_DIR, "KB_CODE_E2E_REPO_DIR not set — global-setup didn't run");

    await page.goto(`${BASE}/r/${REPO_NAME}`);
    await page.locator(".kbc-tree__row", { hasText: KNOWN_FILE }).click();
    await expect(page.locator(".kbc-codeview")).toContainText(KNOWN_SYMBOL, { timeout: 10_000 });

    const chip = page.locator("[data-kbc-ref-chip]");
    await expect(chip).toBeVisible();
    await expect(chip).toContainText("working tree");

    await page.keyboard.press(" ");
    await page.keyboard.press("@");
    const overlay = page.locator("[data-kbc-ref-overlay]");
    await expect(overlay).toBeVisible({ timeout: 5_000 });

    await overlay.locator("input").fill("HEAD~1");
    const opt = overlay.locator("[data-kbc-reftypeahead-opt='HEAD~1']");
    await expect(opt).toBeVisible({ timeout: 5_000 });
    await opt.click();

    await expect(page).toHaveURL(/ref=HEAD~1/);
    await expect(page.locator("[data-kbc-frame-banner]")).toHaveText("file at ref: ODB");
    await expect(page.locator(".kbc-codeview")).toBeVisible();

    await page.locator(".kbc-codeview").click();
    await page.keyboard.press("c");
    await expect(page).toHaveURL(/pane2=/);
    await expect(page.locator("[data-kbc-compare-file]")).toBeVisible({ timeout: 10_000 });
  });
});
