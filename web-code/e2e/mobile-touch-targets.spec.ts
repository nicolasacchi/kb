import { expect, test, type Page } from "@playwright/test";
import { KNOWN_FILE } from "./fixture-repo";
import { BASE, REPO_DIR, REPO_NAME } from "./helpers";

/// v0.44 X2 (A9.f7) - V80-R7's touch-target floor, asserted in a browser.
///
/// R7 swept six pages (Home, Search, Reader, Reviews, Room, review-diff) with
/// `min-height: 44px` / padding + negative-margin hit-slop under
/// `@media (pointer: coarse)`. Nothing measured it, so a later stylesheet
/// (or a lazily loaded chunk winning on injection order, the failure R7
/// itself found) could undo it silently. `hasTouch` + `isMobile` make the
/// pointer coarse, so the real media query applies. Each page is checked on
/// controls R7 names, via their rendered bounding box (hit-slop padding
/// counts: it is part of the box, and is what a finger hits).
test.describe("V80-R7 touch targets are >= 44px on the six named pages (A9.f7)", () => {
  test.use({ viewport: { width: 390, height: 844 }, hasTouch: true, isMobile: true });

  const FLOOR = 44;

  async function expectTapHeight(page: Page, selector: string): Promise<void> {
    const target = page.locator(selector).first();
    await expect(target, `${selector} is rendered`).toBeVisible({ timeout: 15_000 });
    const box = await target.boundingBox();
    expect(box, `${selector} has a box`).not.toBeNull();
    // Sub-pixel layout can land at 43.98; a real regression is far below.
    expect(box!.height, `${selector} height`).toBeGreaterThanOrEqual(FLOOR - 0.5);
  }

  test("coarse pointer is in effect", async ({ page }) => {
    await page.goto(`${BASE}/`);
    expect(await page.evaluate(() => window.matchMedia("(pointer: coarse)").matches)).toBe(true);
  });

  test("Home", async ({ page }) => {
    await page.goto(`${BASE}/`);
    await expectTapHeight(page, ".kbc-home-card__name");
  });

  test("Search", async ({ page }) => {
    await page.goto(`${BASE}/search?q=fixture`);
    await expectTapHeight(page, ".kbc-searchpage__input");
  });

  test("Reader", async ({ page }) => {
    await page.goto(`${BASE}/r/${REPO_NAME}/${KNOWN_FILE}`);
    await expectTapHeight(page, ".kbc-burger");
  });

  test("Reviews list", async ({ page }) => {
    await page.goto(`${BASE}/r/${REPO_NAME}/~reviews`);
    await expectTapHeight(page, ".kbc-reviews__start");
  });

  test("Room and review-diff", async ({ page, request }) => {
    test.skip(!REPO_DIR, "KB_CODE_E2E_REPO_DIR not set - global-setup didn't run");
    const createRes = await request.post(`${BASE}/api/reviews`, {
      data: { repo: REPO_NAME, head_ref: "feature-x", base_ref: "main", title: "e2e touch targets" },
    });
    expect(createRes.ok(), `create review: ${createRes.status()} ${await createRes.text()}`).toBeTruthy();
    const { id } = (await createRes.json()) as { id: number };

    await page.goto(`${BASE}/r/${REPO_NAME}/~reviews/${id}`);
    await expectTapHeight(page, ".kbc-review__back");

    await page.goto(`${BASE}/r/${REPO_NAME}/~reviews/${id}/diff`);
    await expectTapHeight(page, ".kbc-rdiff__back");
  });
});
