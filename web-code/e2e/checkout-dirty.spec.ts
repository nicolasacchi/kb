import { rmSync, writeFileSync } from "node:fs";
import { join } from "node:path";
import { expect, test } from "@playwright/test";
import { OTHER_BRANCH } from "./fixture-repo";
import { BASE, REPO_DIR, REPO_NAME } from "./helpers";

const DIRTY_SCRATCH_FILE = "dirty-scratch.txt";

/// W4.7's confirmed checkout, the DIRTY-refusal path: an untracked file
/// dropped straight onto the fixture repo's working tree (bypassing the
/// SPA entirely, via `REPO_DIR` — see `helpers.ts`'s doc) makes `git status
/// --porcelain` non-empty, so `POST /api/checkout` refuses with a 409
/// carrying `dirty_paths` (`crate::checkout::switch_repo`'s doc) — this
/// spec drives the ref picker's "Switch" action against the fixture's
/// second branch (`OTHER_BRANCH`, added specifically so there's a
/// non-current ref to target) and asserts the SAME dialog surfaces the
/// honest refusal with the actual path list, not a generic error.
test.describe("checkout — dirty refusal", () => {
  // Leave the fixture clean for whatever spec runs next — this suite makes
  // no ordering assumption about itself vs. the others, but there's no
  // reason to leave a stray file behind either.
  test.afterAll(() => {
    if (REPO_DIR) rmSync(join(REPO_DIR, DIRTY_SCRATCH_FILE), { force: true });
  });

  test("409 lists the dirty paths in the same confirm dialog", async ({ page }) => {
    // `test.skip(condition, reason)` INSIDE the test body (not at
    // describe/file scope) — the describe-scope form is evaluated at
    // file-COLLECTION time, which Playwright runs before `globalSetup`, so
    // it would always see `REPO_DIR` unset regardless of what
    // `global-setup.ts` later exports; this form runs at actual test
    // EXECUTION time, in the worker, after globalSetup has set the env var.
    test.skip(!REPO_DIR, "KB_CODE_E2E_REPO_DIR not set — global-setup didn't run");
    writeFileSync(join(REPO_DIR, DIRTY_SCRATCH_FILE), "uncommitted scratch content\n");

    await page.goto(`${BASE}/r/${REPO_NAME}`);
    await page.locator(".kbc-refpicker__trigger").click();

    const branchRow = page.locator(".kbc-refpicker__row", { hasText: OTHER_BRANCH });
    await expect(branchRow).toBeVisible();
    await branchRow.locator("[data-kbc-refpicker-switch]").click();

    const dialog = page.locator("dialog.confirm");
    await expect(dialog).toBeVisible();
    await expect(dialog).toContainText(OTHER_BRANCH);

    const checkoutPromise = page.waitForResponse(
      (res) => res.url().includes("/api/checkout") && res.request().method() === "POST",
    );
    await dialog.locator(".confirm__go").click();
    const checkoutResponse = await checkoutPromise;
    expect(checkoutResponse.status()).toBe(409);

    await expect(dialog.locator("[data-kbc-checkout-dirty]")).toBeVisible();
    const dirtyPaths = dialog.locator("[data-kbc-dirty-path]");
    await expect(dirtyPaths.filter({ hasText: DIRTY_SCRATCH_FILE })).toHaveCount(1);

    // The dialog degrades to a "Close"-only footer once refused — no
    // silent retry-as-if-nothing-happened.
    await expect(dialog.locator(".confirm__go")).toHaveCount(0);
    await dialog.locator(".confirm__cancel").click();
    await expect(dialog).toBeHidden();
  });
});
