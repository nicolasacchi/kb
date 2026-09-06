import { expect, test, type Page } from "@playwright/test";
import { BASE, REPO_NAME } from "./helpers";

/// V70-A6 — root CLAUDE.md #31, ported (D25: "kb's … `useScrollRestoration`
/// ported rather than re-invented"). The recon's G1 was blunt: kb-code had
/// NONE of this, so Back onto any long list page landed at whatever offset the
/// browser guessed.
///
/// TWO THINGS THIS SPEC PINS, and the second is the reason it exists at all:
///
///   1. the offset survives a round trip (navigate away, Back, same place);
///   2. it is the APP SCROLLER's offset, not the window's. kb's SPA
///      window-scrolls; kb-code's does not (`.kbc-app` is `height: 100vh`
///      and `.kbc-approute` is `overflow: auto`, `styles/reader.css`), so a
///      verbatim port would have restored `window.scrollY`, which is
///      permanently 0 here — a no-op that looks like a feature. If someone
///      "simplifies" the hook back to `window.scrollTo`, this fails.

const SCROLLER = ".kbc-approute";

/// A small viewport so the fixture's short lists actually overflow. Without
/// this the assertion would be vacuously true on a page that cannot scroll.
async function openScrollableList(page: Page, url: string) {
  await page.setViewportSize({ width: 520, height: 260 });
  await page.goto(url);
  await expect(page.locator(SCROLLER)).toBeVisible({ timeout: 10_000 });
  // Wait for the list's own content to land, then confirm it really can
  // scroll — a test that silently passes on a 0-height overflow proves nothing.
  await expect
    .poll(
      () =>
        page.evaluate((sel) => {
          const el = document.querySelector(sel);
          return el ? el.scrollHeight - el.clientHeight : 0;
        }, SCROLLER),
      { timeout: 10_000, message: `${url} never became scrollable at 520x260` },
    )
    .toBeGreaterThan(40);
}

async function scrollTo(page: Page, y: number) {
  await page.evaluate(
    ({ sel, top }) => {
      document.querySelector(sel)?.scrollTo({ top });
    },
    { sel: SCROLLER, top: y },
  );
  // The hook persists on a real `scroll` event; give it one frame to land.
  await page.waitForTimeout(120);
}

function scrollOf(page: Page): Promise<number> {
  return page.evaluate((sel) => document.querySelector(sel)?.scrollTop ?? -1, SCROLLER);
}

test.describe("scroll restoration (root CLAUDE.md #31)", () => {
  test("a list page's scroll survives Back", async ({ page }) => {
    const listUrl = `${BASE}/r/${REPO_NAME}/~todos`;
    await openScrollableList(page, listUrl);

    await scrollTo(page, 60);
    expect(await scrollOf(page)).toBeGreaterThan(0);

    // Leave, then come back the way an operator does — the BROWSER's Back,
    // which is the only Back this app has (§P7: "there is no second stack").
    await page.goto(`${BASE}/r/${REPO_NAME}/~hotspots`);
    await expect(page.locator(SCROLLER)).toBeVisible();
    await page.goBack();
    await expect(page).toHaveURL(listUrl);

    await expect
      .poll(() => scrollOf(page), { timeout: 5_000, message: "the offset was not restored" })
      .toBeGreaterThan(0);
  });

  test("the window is NOT the scroll container — the port had to adapt", async ({ page }) => {
    await openScrollableList(page, `${BASE}/r/${REPO_NAME}/~todos`);
    await scrollTo(page, 60);
    // The shell pins the document to the viewport; everything scrolls inside
    // `.kbc-approute`. This is the fact the hook had to be adapted for.
    expect(await page.evaluate(() => window.scrollY)).toBe(0);
    expect(await scrollOf(page)).toBeGreaterThan(0);
  });

  test("history.scrollRestoration is manual, so the browser never fights the hook", async ({
    page,
  }) => {
    await page.goto(`${BASE}/r/${REPO_NAME}/~todos`);
    expect(await page.evaluate(() => history.scrollRestoration)).toBe("manual");
  });

  test("an ephemeral param does not fragment one list's slot", async ({ page }) => {
    // `?focus=`/`?thread=`/`?finding=` open a panel or flash a row ON TOP of
    // an unchanged list (`LIST_EPHEMERAL_PARAMS`). Two URLs differing only in
    // one of them must share a slot, or every tap would start a fresh one.
    const listUrl = `${BASE}/r/${REPO_NAME}/~todos`;
    await openScrollableList(page, `${listUrl}?focus=a`);
    await scrollTo(page, 60);
    await page.goto(`${BASE}/r/${REPO_NAME}/~hotspots`);
    await page.goto(`${listUrl}?focus=b`);
    await expect
      .poll(() => scrollOf(page), { timeout: 5_000 })
      .toBeGreaterThan(0);
  });
});
