import { expect, test } from "@playwright/test";
import { KNOWN_FILE, KNOWN_SYMBOL } from "./fixture-repo";
import { BASE, REPO_NAME } from "./helpers";

/// V3.1-H3a — param-name inlay hints at literal call-site args.
/// `helper()` in lib.rs calls `KNOWN_SYMBOL(1, 2)` — two literal args whose
/// signature is `fn omniboxTargetFunction(a: i32, b: i32)`.

async function openKnownFile(page: import("@playwright/test").Page) {
  await page.goto(`${BASE}/r/${REPO_NAME}`);
  await page.locator(".kbc-tree__row", { hasText: KNOWN_FILE }).click();
  await expect(page.locator(".kbc-codeview")).toContainText(KNOWN_SYMBOL, { timeout: 10_000 });
}

test.describe("param hints (V3.1-H3a)", () => {
  test("literal args at a resolvable call site show name widgets", async ({ page }) => {
    await openKnownFile(page);
    // Focus the buffer so the viewport plugin runs.
    await page.locator(".kbc-codeview").click();

    // Wait for at least one param-hint widget (debounced resolve).
    const hint = page.locator("[data-kbc-param-hint]").first();
    await expect(hint).toBeVisible({ timeout: 15_000 });
    // Expect a param name from the known signature (a or b).
    const name = await hint.getAttribute("data-kbc-param-hint");
    expect(name === "a" || name === "b").toBe(true);
  });

  test("toggling the Pref hides param hints", async ({ page }) => {
    await openKnownFile(page);
    await page.locator(".kbc-codeview").click();
    await expect(page.locator("[data-kbc-param-hint]").first()).toBeVisible({ timeout: 15_000 });

    const toggle = page.locator("[data-kbc-param-hints-toggle]");
    await expect(toggle).toBeVisible();
    await toggle.click();
    await expect(toggle).toHaveAttribute("aria-pressed", "false");

    // After disable + debounce, widgets should clear.
    await expect(page.locator("[data-kbc-param-hint]")).toHaveCount(0, { timeout: 5_000 });
  });
});
