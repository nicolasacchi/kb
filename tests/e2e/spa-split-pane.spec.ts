import { test, expect, type Page } from "@playwright/test";
import { BASE } from "./helpers";

// W3.P-b — the two-pane artifact COMPARE split.
//
// What this file is really guarding:
//   * `?pane2=` is the WHOLE state. There is no separate "is a split open"
//     React flag, so a bare URL must reconstitute the split (test 1) and the
//     verb must round-trip through the URL (test 2).
//   * the two panes must be TWO DIFFERENT ARTIFACT ORIGINS. That is the
//     invariant the whole design rests on: `isOriginOfArtifact` is the only
//     guard that can attribute a `kb:scroll`/`kb:reading` beacon to one pane's
//     append-only visit row (#8/#19), and it works on the origin. Two panes
//     sharing an origin would silently cross-post reading history.
//   * exactly ONE inspector rail (`.kb-pinsp`) for the whole reader —
//     invariant #30's "one home per action, ONE merged rail". ContextBar IS
//     duplicated per pane (it is per-artifact chrome); the rail is not.
//   * mobile NEVER splits (the v0.23 ONE-button/ONE-sheet contract).
//
// The canon corpus's root folder holds four flat artifacts
// (cost-of-abstraction / fullscreen-viz / kitchen-sink / multi-page), so
// "open beside" — the next direct sibling, filename-sorted, wrapping — always
// has a target from kitchen-sink.html.

const PRIMARY = "/a/canon/kitchen-sink.html";
// The raw `formatPane2` value is `canon:multi-page.html`; `artifactHref`'s
// single encodeURIComponent is what puts it on the wire.
const PANE2_PARAM = "canon%3Amulti-page.html";

const frames = (page: Page) => page.locator("iframe.detail__frame");
const panes = (page: Page) => page.locator(".detail__pane");
const rails = (page: Page) => page.locator(".kb-pinsp");

async function frameSrcs(page: Page): Promise<string[]> {
  return frames(page).evaluateAll((els) =>
    els.map((e) => (e as HTMLIFrameElement).src),
  );
}

test.describe("two-pane split (desktop)", () => {
  test("?pane2= reconstitutes two panes with DIFFERENT artifact origins, and still exactly one inspector rail", async ({
    page,
  }) => {
    await page.goto(`${BASE}${PRIMARY}?pane2=${PANE2_PARAM}`);

    await expect(page.locator(".detail__panes")).toBeVisible();
    await expect(panes(page)).toHaveCount(2);
    await expect(frames(page)).toHaveCount(2);

    const srcs = await frameSrcs(page);
    expect(srcs).toHaveLength(2);
    const origins = srcs.map((s) => new URL(s).origin);
    // Both are artifact subdomains…
    for (const o of origins) expect(o).toContain(".artifacts.");
    // …and they are DIFFERENT ones (the attribution invariant, #8/#19).
    expect(origins[0]).not.toBe(origins[1]);

    // ONE merged inspector rail for the whole reader (#30) — never one per
    // pane. ContextBar, by contrast, IS per-pane chrome.
    await expect(rails(page)).toHaveCount(1);
    await expect(page.locator(".kb-ctxbar")).toHaveCount(2);
  });

  test("the ContextBar split verb opens beside, and pane 2's verb closes it", async ({
    page,
  }) => {
    await page.goto(`${BASE}${PRIMARY}`);
    await expect(frames(page)).toHaveCount(1);
    await expect(page.locator(".detail__panes")).toHaveCount(0);

    // "open beside" lives on the primary pane only (and only once the
    // sibling walk has resolved a target).
    const splitBtn = page.locator('[data-kb-act="split"]');
    await expect(splitBtn).toHaveCount(1);
    await splitBtn.click();

    await expect(frames(page)).toHaveCount(2);
    await expect(page).toHaveURL(/[?&]pane2=/);
    await expect(rails(page)).toHaveCount(1);

    const origins = (await frameSrcs(page)).map((s) => new URL(s).origin);
    expect(origins[0]).not.toBe(origins[1]);

    // The surviving `[data-kb-act="split"]` is now pane 2's "close this
    // pane" (the primary drops its own while a split is open).
    const closeBtn = page.locator('[data-kb-pane="2"] [data-kb-act="split"]');
    await expect(closeBtn).toHaveCount(1);
    await closeBtn.click();

    await expect(frames(page)).toHaveCount(1);
    await expect(page.locator(".detail__panes")).toHaveCount(0);
    await expect(page).not.toHaveURL(/[?&]pane2=/);
  });

  test("the `w` pane chord splits (w v) and closes (w q)", async ({ page }) => {
    await page.goto(`${BASE}${PRIMARY}`);
    // Wait for the sibling walk (the split target) to resolve.
    await expect(page.locator('[data-kb-act="split"]')).toHaveCount(1);

    await page.keyboard.press("w");
    await page.keyboard.press("v");
    await expect(frames(page)).toHaveCount(2);
    await expect(page).toHaveURL(/[?&]pane2=/);

    await page.keyboard.press("w");
    await page.keyboard.press("q");
    await expect(frames(page)).toHaveCount(1);
    await expect(page).not.toHaveURL(/[?&]pane2=/);
  });

  test("split toggles are replace:true — one back step still leaves the reader", async ({
    page,
  }) => {
    await page.goto(`${BASE}/`);
    await page.goto(`${BASE}${PRIMARY}`);
    await expect(page.locator('[data-kb-act="split"]')).toHaveCount(1);
    await page.locator('[data-kb-act="split"]').click();
    await expect(frames(page)).toHaveCount(2);

    // If the split had pushed a history entry, this would land back on the
    // single-pane reader instead of the gallery.
    await page.goBack();
    await expect(page).toHaveURL(/\/(\?|$)/);
  });
});

test.describe("mobile never splits", () => {
  // Exactly the breakpoint (`useIsMobile` is `max-width: 860px`, inclusive).
  test.use({ viewport: { width: 860, height: 720 } });

  test("?pane2= renders a single pane at 860px", async ({ page }) => {
    await page.goto(`${BASE}${PRIMARY}?pane2=${PANE2_PARAM}`);
    await expect(frames(page)).toHaveCount(1);
    await expect(page.locator(".detail__panes")).toHaveCount(0);
    // The param survives untouched, so widening the viewport restores it.
    await expect(page).toHaveURL(/[?&]pane2=/);
  });
});
