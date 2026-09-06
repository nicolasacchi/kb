import { expect, test } from "@playwright/test";
import { KNOWN_SYMBOL } from "./fixture-repo";
import { BASE, REPO_NAME } from "./helpers";

/// V3.4-C2 — working-set canvas: list → create → add fragment via search →
/// drag → PUT persists across reload → delete. Mutations are loopback-only
/// server-side; the e2e harness hits 127.0.0.1 so create/save/delete work.
/// No new commits/branches — uses the existing fixture symbol.

test.describe("working-set canvas", () => {
  test("list, create, add fragment, drag, persist, delete", async ({ page }) => {
    await page.goto(`${BASE}/r/${REPO_NAME}/~canvas`);
    await expect(page.locator("[data-kbc-canvas]")).toBeVisible({ timeout: 10_000 });
    // An EMPTY <ul> has zero height => Playwright "hidden"; before any
    // canvas exists attached is the honest assertion (creation follows).
    await expect(page.locator("[data-kbc-canvas-list]")).toBeAttached();

    const canvasName = `e2e-canvas-${Date.now()}`;
    await page.locator("[data-kbc-canvas-create-name]").fill(canvasName);
    await page.locator("[data-kbc-canvas-create-submit]").click();

    // Create navigates to ?id=…
    await expect(page).toHaveURL(/~canvas\?id=\d+/, { timeout: 10_000 });
    await expect(page.locator("[data-kbc-canvas-surface]")).toBeVisible({ timeout: 10_000 });
    await expect(page.locator("[data-kbc-canvas-row]").filter({ hasText: canvasName })).toBeVisible();

    // Add a fragment via symbol search (existing fixture symbol).
    await page.locator("[data-kbc-canvas-add-symbol]").click();
    await page.locator("[data-kbc-canvas-search]").fill(KNOWN_SYMBOL);
    const hit = page.locator("[data-kbc-canvas-search-hit]").filter({ hasText: KNOWN_SYMBOL }).first();
    await expect(hit).toBeVisible({ timeout: 15_000 });
    await hit.click();

    const card = page.locator(`[data-kbc-canvas-card-symbol="${KNOWN_SYMBOL}"]`);
    await expect(card).toBeVisible({ timeout: 10_000 });
    // Source body or loading → eventually body (or stale if index lag).
    await expect(
      card.locator("[data-kbc-canvas-card-body], [data-kbc-canvas-card-stale]"),
    ).toBeVisible({ timeout: 15_000 });

    // Drag the card head by mouse steps; position should change.
    const head = card.locator("[data-kbc-canvas-card-head]");
    const before = await card.boundingBox();
    expect(before).toBeTruthy();
    await head.hover();
    await page.mouse.down();
    await page.mouse.move(before!.x + before!.width / 2 + 120, before!.y + 10, { steps: 8 });
    await page.mouse.up();
    // Wait for debounced PUT (≥1s) + dirty→saved.
    await expect(page.locator("[data-kbc-canvas-save-state]")).toHaveText("saved", {
      timeout: 8_000,
    });

    const afterDrag = await card.boundingBox();
    expect(afterDrag).toBeTruthy();
    // Card moved in screen space (pan unchanged, so box delta ≈ drag).
    expect(Math.abs((afterDrag!.x - before!.x) + (afterDrag!.y - before!.y))).toBeGreaterThan(20);

    // Reload keeps the fragment position.
    await page.reload();
    await expect(page.locator(`[data-kbc-canvas-card-symbol="${KNOWN_SYMBOL}"]`)).toBeVisible({
      timeout: 15_000,
    });
    const afterReload = await page
      .locator(`[data-kbc-canvas-card-symbol="${KNOWN_SYMBOL}"]`)
      .boundingBox();
    expect(afterReload).toBeTruthy();
    // Positions should match within a few px of layout reflow.
    expect(Math.abs(afterReload!.x - afterDrag!.x)).toBeLessThan(30);
    expect(Math.abs(afterReload!.y - afterDrag!.y)).toBeLessThan(30);

    // Delete canvas.
    const row = page.locator("[data-kbc-canvas-row]").filter({ hasText: canvasName });
    await row.locator("[data-kbc-canvas-delete]").click();
    await page.locator(".confirm__go").click();
    await expect(page.locator("[data-kbc-canvas-row]").filter({ hasText: canvasName })).toHaveCount(
      0,
      { timeout: 10_000 },
    );
  });
});
