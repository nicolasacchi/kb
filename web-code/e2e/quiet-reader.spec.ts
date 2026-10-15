import { expect, test } from "@playwright/test";
import { BASE, REPO_DIR, REPO_NAME } from "./helpers";
import { KNOWN_FILE } from "./fixture-repo";

/// OL-C11 — the quiet reader: open a file and the code is the page. One top
/// bar (the path is its breadcrumb), one pane header row, then code.

test.describe("Quiet reader (OL-C11)", () => {
  test.use({ viewport: { width: 1280, height: 720 } });
  test.beforeEach(() => {
    test.skip(!REPO_DIR, "KB_CODE_E2E_REPO_DIR not set — global-setup didn't run");
  });

  test("breadcrumbs live in the top bar, not above the code", async ({ page }) => {
    await page.goto(`${BASE}/r/${REPO_NAME}/${KNOWN_FILE}?desk=read`);
    await expect(page.locator(".kbc-codeview")).toBeVisible({ timeout: 15_000 });
    const crumbs = page.locator('nav.kbc-breadcrumbs[aria-label="Breadcrumb"]');
    await expect(crumbs).toHaveCount(1);
    await expect(page.locator('[data-region="topbar"] nav.kbc-breadcrumbs')).toBeVisible();
    await expect(page.locator(".kbc-reader__top")).toBeHidden();
  });

  test("one pane header row: history arrows, filename, toolbar — and the code starts high", async ({ page }) => {
    await page.goto(`${BASE}/r/${REPO_NAME}/${KNOWN_FILE}?desk=read`);
    await expect(page.locator(".kbc-codeview")).toBeVisible({ timeout: 15_000 });

    // The one-file working set is the filename, not a second chip row.
    await expect(page.locator(".kbc-ws")).toHaveCount(0);

    const head = page.locator('[data-kbc-panechrome="1"]');
    await expect(head.locator("[data-kbc-panehist]")).toBeVisible();
    await expect(head.locator("[data-kbc-panechrome-file]")).toHaveText(KNOWN_FILE.split("/").pop()!);
    const bar = head.locator("[data-kb-viewbar]");
    await expect(bar).toBeVisible();
    await expect(bar).toHaveAttribute("aria-label", "Reader view");
    await expect(bar.locator(".kb-viewbar__title")).toHaveCount(0);

    // Nothing clips: the toolbar ends inside its row, inside the viewport.
    const box = async (loc: ReturnType<typeof page.locator>) => (await loc.boundingBox())!;
    const hb = await box(head);
    const bb = await box(bar);
    expect(bb.x + bb.width, "toolbar overflows the pane header row").toBeLessThanOrEqual(hb.x + hb.width + 1);
    const refGroup = await box(head.locator('[role="group"][aria-label="Ref"]'));
    expect(refGroup.x + refGroup.width, "ref group overflows the pane header row").toBeLessThanOrEqual(
      hb.x + hb.width + 1,
    );

    // Header sits directly under the top bar; the code follows it. Before
    // OL-C11 the editor started ~147px down (three chrome rows).
    const top = await page.locator(".cm-scroller").evaluate((el) => el.getBoundingClientRect().top);
    console.log(`quiet-reader: code scroller top y = ${top.toFixed(0)}px`);
    expect(top).toBeLessThan(100);
  });

  test("overflow toggles are one click away in More", async ({ page }) => {
    await page.goto(`${BASE}/r/${REPO_NAME}/${KNOWN_FILE}?desk=read`);
    await expect(page.locator(".kbc-codeview")).toBeVisible({ timeout: 15_000 });
    await page.locator("[data-kb-viewbar-more]").click();
    for (const attr of ["wrap", "param-hints", "lenses", "sticky"]) {
      await expect(page.locator(`[data-kbc-${attr}-toggle]`)).toBeVisible();
    }
  });

  test("the top bar hint is two chips with the sentence as tooltip", async ({ page }) => {
    await page.goto(`${BASE}/r/${REPO_NAME}/${KNOWN_FILE}?desk=read`);
    const hint = page.locator("[data-kbc-spacehint]");
    await expect(hint).toBeVisible();
    await expect(hint).toHaveAttribute("title", /for everything.*for keys/);
    await expect(hint.locator("kbd")).toHaveCount(2);
    await expect(hint).not.toContainText("for everything");
  });
});
