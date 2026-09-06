import { expect, test } from "@playwright/test";
import { KNOWN_FILE, KNOWN_SYMBOL } from "./fixture-repo";
import { BASE, REPO_NAME } from "./helpers";

/// V3.R2 / R12 — blame originating-change section. Fixture repo has real
/// commits; enabling provenance dots + opening the why-panel on a blamed
/// line should surface the "Originating change" section (lazy /api/diff).
test.describe("blame hover-diff (R12)", () => {
  test("originating-change section appears for a committed line", async ({ page }) => {
    // Same SPA-shell navigation pattern as blame-gutter.spec.ts (dot-extension
    // hard nav 404s on the daemon's asset-vs-shell split).
    await page.goto(`${BASE}/r/${REPO_NAME}`);
    await page.locator(".kbc-tree__row", { hasText: KNOWN_FILE }).click();
    await expect(page.locator(".kbc-codeview")).toContainText(KNOWN_SYMBOL, { timeout: 10_000 });

    const toggle = page.locator("[data-kbc-provenance-toggle]");
    await expect(toggle).toBeVisible();
    const blamePromise = page.waitForResponse((res) => res.url().includes("/api/blame?"));
    const whyPrefetchPromise = page.waitForResponse((res) => res.url().includes("/api/why?"));
    await toggle.click();
    await blamePromise;
    await whyPrefetchPromise;

    // Click line 2's gutter (same coordinate-delegation as blame-gutter.spec).
    const lineTwo = page.locator(".cm-lineNumbers .cm-gutterElement", { hasText: /^2$/ }).first();
    const lineBox = await lineTwo.boundingBox();
    const blameGutter = page.locator(".cm-gutter.kbc-blame-gutter");
    const gutterBox = await blameGutter.boundingBox();
    expect(lineBox).not.toBeNull();
    expect(gutterBox).not.toBeNull();
    await page.mouse.click(gutterBox!.x + gutterBox!.width / 2, lineBox!.y + lineBox!.height / 2);

    const panel = page.locator('[data-kbc-why-line="2"]');
    await expect(panel).toBeVisible({ timeout: 10_000 });

    // Originating-change section mounts and either loads a diff, reports
    // empty/no-overlap, or shows the uncommitted absence arm — never blank.
    const origin = panel.locator("[data-kbc-why-origin]");
    await expect(origin).toBeVisible({ timeout: 10_000 });
    // For a committed fixture line the section should attempt a diff fetch;
    // wait until loading settles into one of the terminal states.
    await expect(
      origin.locator(
        "[data-kbc-why-origin-diff], [data-kbc-why-origin-empty], [data-kbc-why-origin-error], [data-kbc-why-origin=\"uncommitted\"]",
      ),
    ).toBeVisible({ timeout: 15_000 });
  });
});
