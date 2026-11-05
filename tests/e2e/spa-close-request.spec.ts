import { test, expect, type Page } from "@playwright/test";
import { BASE, installCloseWatcherStub } from "./helpers";

// AH4 — mobile sheets and drawers answer the platform close request
// (Android Back / swipe-back) through `useCloseRequest`, with no history
// entry. Real Back cannot be driven here, so a CloseWatcher stub with the
// platform's semantics stands in (helpers.ts); `__cw.live` counts watchers
// that have not been destroyed.

const ARTIFACT = "/a/canon/kitchen-sink.html";

const cw = (page: Page) =>
  page.evaluate(() => {
    const c = (window as unknown as { __cw: { made: number; live: number } }).__cw;
    return { made: c.made, live: c.live };
  });
const live = (page: Page) => cw(page).then((c) => c.live);
const requestClose = (page: Page) =>
  page.evaluate(() =>
    (
      window as unknown as { __cw: { instances: { requestClose(): void }[] } }
    ).__cw.instances
      .filter((w) => (w as unknown as { alive: boolean }).alive)
      .at(-1)!
      .requestClose(),
  );

test.describe("close request: reader sheet and nav sheets (390x844)", () => {
  test.use({ viewport: { width: 390, height: 844 }, hasTouch: true });

  // invariant:30
  test("reader sheet: Back closes it, no history entry; X and scrim release the watcher", async ({
    page,
  }) => {
    await installCloseWatcherStub(page);
    await page.goto(`${BASE}${ARTIFACT}`);
    const detail = page.locator(".detail");
    await expect(page.locator('[data-kb-act="inspect"]')).toBeVisible();
    // Nothing is watched with no sheet open, so Back still leaves.
    expect((await cw(page)).made).toBe(0);
    const len = await page.evaluate(() => history.length);

    await page.locator('[data-kb-act="inspect"]').click();
    await expect(detail).toHaveClass(/detail--inspector-open/);
    expect(await live(page)).toBe(1);
    await requestClose(page);
    await expect(detail).not.toHaveClass(/detail--inspector-open/);
    await expect(page.locator(".kb-pinsp")).toBeHidden();
    expect(await live(page)).toBe(0);
    expect(await page.evaluate(() => history.length)).toBe(len);

    // X
    await page.locator('[data-kb-act="inspect"]').click();
    await expect(detail).toHaveClass(/detail--inspector-open/);
    await page.locator(".kb-pinsp__sheet-x").click();
    await expect(detail).not.toHaveClass(/detail--inspector-open/);
    expect(await live(page)).toBe(0);

    // scrim
    await page.locator('[data-kb-act="inspect"]').click();
    await expect(detail).toHaveClass(/detail--inspector-open/);
    await page.locator(".kb-pinsp-scrim").click({ position: { x: 195, y: 24 } });
    await expect(detail).not.toHaveClass(/detail--inspector-open/);
    expect(await live(page)).toBe(0);
  });

  // invariant:30
  test("a focused artifact iframe is blurred when the sheet opens from inside it", async ({
    page,
  }) => {
    await installCloseWatcherStub(page);
    await page.goto(`${BASE}${ARTIFACT}`);
    const frame = page.frameLocator(".detail__frame");
    await frame.locator("h1").first().click();
    expect(await page.evaluate(() => document.activeElement?.tagName)).toBe("IFRAME");

    await frame.locator(":root").evaluate(() => {
      window.parent.postMessage({ type: "cm:focus", commentId: "ah4-probe" }, "*");
    });
    await expect(page.locator(".detail")).toHaveClass(/detail--inspector-open/);
    expect(await live(page)).toBe(1);
    await expect
      .poll(() => page.evaluate(() => document.activeElement?.tagName))
      .not.toBe("IFRAME");
  });

  test("kb NavSheet answers the close request", async ({ page }) => {
    await installCloseWatcherStub(page);
    await page.goto(`${BASE}/`);
    await page.locator("[data-kb-spine-menu]").click();
    await expect(page.locator("[data-kb-sheet]")).toBeVisible();
    expect(await live(page)).toBe(1);
    await requestClose(page);
    await expect(page.locator("[data-kb-sheet]")).toHaveCount(0);
    expect(await live(page)).toBe(0);
  });

  test("search filters drawer answers the close request", async ({ page }) => {
    await installCloseWatcherStub(page);
    await page.goto(`${BASE}/search?q=kitchen`);
    await page.locator(".kb-search__filters-btn").click();
    const drawer = page.locator('[role=dialog][aria-label="search filters"]');
    await expect(drawer).toBeVisible();
    expect(await live(page)).toBe(1);
    await requestClose(page);
    await expect(drawer).toHaveCount(0);
    expect(await live(page)).toBe(0);
  });

  test("slate composer and history sheets answer the close request", async ({ page, request }) => {
    await installCloseWatcherStub(page);
    const tag = Date.now().toString(36);
    const slug = `e2e-close-request-${tag}`;
    // One post through the slate's one write route, so the board renders.
    const r = await request.post(`${BASE}/api/slates/${slug}/posts`, {
      data: {
        prov: { harness: "codex", origin: "agent", session_id: `e2e-close-request-${tag}` },
        kind: "now",
        line: "close request probe",
      },
      headers: { "Content-Type": "application/json" },
    });
    expect(r.status(), await r.text()).toBeLessThan(300);
    await page.goto(`${BASE}/slates/${slug}`);
    await expect(page.getByRole("status", { name: /now and warnings/i })).toBeVisible({
      timeout: 15_000,
    });
    expect((await cw(page)).made).toBe(0);
    const len = await page.evaluate(() => history.length);

    await page.locator('[data-act="compose"]').click();
    const composer = page.locator("#slate-composer");
    await expect(composer).toBeVisible();
    expect(await live(page)).toBe(1);
    await requestClose(page);
    await expect(composer).toBeHidden();
    expect(await live(page)).toBe(0);

    await page.locator('[data-act="history"]').click();
    const hist = page.locator("#slate-history");
    await expect(hist).toBeVisible();
    expect(await live(page)).toBe(1);
    await requestClose(page);
    await expect(hist).toBeHidden();
    await expect(page).not.toHaveURL(/history=1/);
    expect(await live(page)).toBe(0);
    expect(await page.evaluate(() => history.length)).toBe(len);
  });
});

test.describe("close request: desktop parity (1280x720)", () => {
  test.use({ viewport: { width: 1280, height: 720 } });

  test("the docked rail never creates a watcher", async ({ page }) => {
    await installCloseWatcherStub(page);
    await page.goto(`${BASE}${ARTIFACT}`);
    await page.locator('[data-kb-act="dock-versions"]').click();
    expect((await cw(page)).made).toBe(0);
  });
});
