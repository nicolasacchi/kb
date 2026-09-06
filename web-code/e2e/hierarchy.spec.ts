import { expect, test } from "@playwright/test";
import {
  CALLER_FILE,
  HIER_TRAIT_FILE,
  HIER_TRAIT_NAME,
  KNOWN_FILE,
  KNOWN_SYMBOL,
} from "./fixture-repo";
import { BASE, REPO_NAME } from "./helpers";

/// V3.1-H3a — call hierarchy (`gc`/`gC`) + type hierarchy (`gt`).
/// Uses the fixture's known call sites and the additive trait/impl pair
/// in `hier_trait.rs` (new commit only — initial commit untouched).

async function openFile(page: import("@playwright/test").Page, file: string, needle: string) {
  await page.goto(`${BASE}/r/${REPO_NAME}`);
  await page.locator(".kbc-tree__row", { hasText: file }).click();
  await expect(page.locator(".kbc-codeview")).toContainText(needle, { timeout: 10_000 });
}

test.describe("hierarchy (V3.1-H3a)", () => {
  test("gc on a known call site opens callers panel with ≥1 row + class badge", async ({
    page,
  }) => {
    // Call site in caller.rs — SPA resolves to the def then lists callers.
    await openFile(page, CALLER_FILE, KNOWN_SYMBOL);
    await page.locator(".kbc-codeview").getByText(KNOWN_SYMBOL, { exact: true }).first().click();

    await page.keyboard.press("g");
    await page.keyboard.press("c");

    const panel = page.locator("[data-kbc-hierarchy]");
    await expect(panel).toBeVisible({ timeout: 10_000 });
    await expect(panel).toHaveAttribute("data-kbc-hier-mode", "callers");

    // At least one non-root edge row with a class badge.
    const rows = panel.locator("[data-kbc-hier-row]");
    await expect(rows.first()).toBeVisible({ timeout: 10_000 });
    const rowCount = await rows.count();
    expect(rowCount).toBeGreaterThanOrEqual(2); // root + ≥1 caller
    await expect(panel.locator("[data-kbc-hier-class]").first()).toBeVisible();

    // Expand a child (▸ / l) — second level fetch (may be empty but must not crash).
    // Move to first non-root row and press l.
    await page.keyboard.press("j");
    await page.keyboard.press("l");
    // Panel stays open; expand either loads children or marks leaf.
    await expect(panel).toBeVisible();

    await page.keyboard.press("Escape");
    await expect(panel).toHaveCount(0);
  });

  test("gC on the known function def opens callees panel", async ({ page }) => {
    // helper() in lib.rs calls KNOWN_SYMBOL — open def of helper via KNOWN_FILE.
    await openFile(page, KNOWN_FILE, "helper");
    await page.locator(".kbc-codeview").getByText("helper", { exact: true }).first().click();

    await page.keyboard.press("g");
    await page.keyboard.press("C");

    const panel = page.locator("[data-kbc-hierarchy]");
    await expect(panel).toBeVisible({ timeout: 10_000 });
    await expect(panel).toHaveAttribute("data-kbc-hier-mode", "callees");
    await expect(panel.locator("[data-kbc-hier-class]").first()).toBeVisible();

    await page.keyboard.press("Escape");
    await expect(panel).toHaveCount(0);
  });

  test("gt on the fixture trait shows implementors", async ({ page }) => {
    await openFile(page, HIER_TRAIT_FILE, HIER_TRAIT_NAME);
    await page.locator(".kbc-codeview").getByText(HIER_TRAIT_NAME, { exact: true }).first().click();

    await page.keyboard.press("g");
    await page.keyboard.press("t");

    const panel = page.locator("[data-kbc-hierarchy]");
    await expect(panel).toBeVisible({ timeout: 10_000 });
    await expect(panel).toHaveAttribute("data-kbc-hier-mode", "types");
    // Subtypes / implementors section or Circle row.
    await expect(panel).toContainText(/Circle|Subtypes|implementors/i, { timeout: 10_000 });
    await expect(panel.locator("[data-kbc-hier-class]").first()).toBeVisible();

    await page.keyboard.press("Escape");
    await expect(panel).toHaveCount(0);
  });
});
