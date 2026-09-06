import { expect, test, type Page } from "@playwright/test";
import { BASE, REPO_DIR, REPO_NAME } from "./helpers";
import { KNOWN_FILE, KNOWN_SYMBOL } from "./fixture-repo";

/// V70-A4 — the Desk's behavioural suite: resize + persistence,
/// collapse-to-stripe, the re-cut rail (All first, passport cards on
/// every tab, caret-follow + pin), the drawer's "Keep in drawer" tenant,
/// and the `?shell=legacy` escape hatch.
///
/// The two GOLDENS live in their own files (`desk-landmarks.spec.ts`,
/// `desk-viewport.spec.ts`) because they are contracts rather than
/// features; this file is everything else the unit shipped.

const READER = `${BASE}/r/${REPO_NAME}/${KNOWN_FILE}`;

async function openReader(page: Page, query = ""): Promise<void> {
  await page.goto(`${READER}${query}`);
  await expect(page.locator(".kbc-codeview")).toBeVisible({ timeout: 15_000 });
}

async function regionWidth(page: Page, region: string): Promise<number> {
  const box = await page.locator(`[data-region="${region}"]`).boundingBox();
  return box?.width ?? 0;
}

test.describe("Desk — resizing (V70-A4)", () => {
  test.use({ viewport: { width: 1280, height: 720 } });
  test.beforeEach(() => {
    test.skip(!REPO_DIR, "KB_CODE_E2E_REPO_DIR not set — global-setup didn't run");
  });

  test("every separator is a real, focusable window splitter", async ({ page }) => {
    await openReader(page);
    // The library renders WAI-ARIA window splitters; three separators —
    // dock|center, main|drawer, center|rail.
    for (const sep of ["dock", "drawer", "rail"]) {
      const el = page.locator(`[data-desk-sep="${sep}"]`);
      await expect(el).toHaveCount(1);
      await expect(el).toHaveAttribute("role", "separator");
    }
  });

  test("dragging the dock separator resizes it, and the size survives a reload", async ({ page }) => {
    await openReader(page);
    const before = await regionWidth(page, "dock");
    expect(before).toBeGreaterThan(50);

    const sep = page.locator('[data-desk-sep="dock"]');
    const box = (await sep.boundingBox())!;
    await page.mouse.move(box.x + box.width / 2, box.y + box.height / 2);
    await page.mouse.down();
    // While the pointer is down the resize cursor lives on ONE dedicated
    // overlay — never on `document.body` (the Lumino/Chromium
    // global-selector trap, jupyterlab/lumino#450).
    await page.mouse.move(box.x + 140, box.y + box.height / 2, { steps: 12 });
    await expect(page.locator("[data-desk-cursor-overlay]")).toHaveCount(1);
    await page.mouse.up();
    await expect(page.locator("[data-desk-cursor-overlay]")).toHaveCount(0);

    await expect.poll(() => regionWidth(page, "dock")).toBeGreaterThan(before + 60);
    const after = await regionWidth(page, "dock");

    await page.reload();
    await expect(page.locator(".kbc-codeview")).toBeVisible({ timeout: 15_000 });
    // Persisted per repo under `kbc:desk:<repo>`, read synchronously
    // before first paint.
    await expect.poll(() => regionWidth(page, "dock")).toBeGreaterThan(after - 24);
    await expect.poll(() => regionWidth(page, "dock")).toBeLessThan(after + 24);

    // A human drag marks the desk dirty; a preset must not silently
    // override it (the "sticky manual deviation" rule).
    await expect(page.locator("[data-desk-dirty]")).toHaveAttribute("data-desk-dirty", "1");
    await expect(page.locator("[data-desk-preset-chip]")).toContainText("edited");
  });

  test("the keyboard resize submode nudges the region graph and shows a mode chip", async ({ page }) => {
    await openReader(page);
    await page.locator(".cm-content").click();
    const before = await regionWidth(page, "rail");

    // Ctrl-w r — the chord machine's own new entry (`editor/vimKeys.ts`).
    await page.keyboard.down("Control");
    await page.keyboard.press("w");
    await page.keyboard.up("Control");
    await page.keyboard.press("r");
    await expect(page.locator("[data-desk-mode-chip]")).toBeVisible();

    // Focus is in main, so `l` (right) shrinks the RAIL — the boundary
    // that lies that way. Direction resolves against the region graph,
    // not the focused element's own edges.
    for (let i = 0; i < 4; i++) await page.keyboard.press("l");
    await expect.poll(() => regionWidth(page, "rail")).toBeLessThan(before - 10);

    // `=` equalises back to the preset; Esc leaves the submode.
    await page.keyboard.press("=");
    await expect.poll(() => regionWidth(page, "rail")).toBeGreaterThan(before - 12);
    await page.keyboard.press("Escape");
    await expect(page.locator("[data-desk-mode-chip]")).toHaveCount(0);
  });

  test("double-clicking a separator resets that region to the preset size", async ({ page }) => {
    await openReader(page);
    const preset = await regionWidth(page, "dock");

    const sep = page.locator('[data-desk-sep="dock"]');
    const box = (await sep.boundingBox())!;
    await page.mouse.move(box.x + box.width / 2, box.y + box.height / 2);
    await page.mouse.down();
    await page.mouse.move(box.x + 150, box.y + box.height / 2, { steps: 10 });
    await page.mouse.up();
    await expect.poll(() => regionWidth(page, "dock")).toBeGreaterThan(preset + 60);

    await sep.dblclick();
    await expect.poll(() => regionWidth(page, "dock")).toBeLessThan(preset + 24);
  });
});

test.describe("Desk — collapse to a stripe (V70-A4)", () => {
  test.use({ viewport: { width: 1280, height: 720 } });
  test.beforeEach(() => {
    test.skip(!REPO_DIR, "KB_CODE_E2E_REPO_DIR not set — global-setup didn't run");
  });

  test("the rail collapses to its badged stripe and comes back on the same button", async ({ page }) => {
    await openReader(page);
    expect(await regionWidth(page, "rail")).toBeGreaterThan(80);

    // The stripe button for the ACTIVE tab toggles the rail shut.
    await page.locator('[data-desk-stripe-btn="all"]').click();
    await expect.poll(() => regionWidth(page, "rail")).toBeGreaterThan(80);

    // Collapsing goes through the region toggle in the rail's own stripe
    // — clicking a tab that is already active and expanded is a no-op by
    // design (it selects, it does not toggle). Use the dock's own button
    // for the collapse contract, which is symmetric.
    await page.locator('[data-desk-stripe-btn="dock"]').click();
    await expect.poll(() => regionWidth(page, "dock")).toBeLessThan(4);
    await expect(page.locator('[data-region="stripe-left"]')).toBeVisible();

    await page.locator('[data-desk-stripe-btn="dock"]').click();
    await expect.poll(() => regionWidth(page, "dock")).toBeGreaterThan(80);
  });

  test("collapse survives a reload — a collapsed region is remembered state", async ({ page }) => {
    await openReader(page);
    await page.locator('[data-desk-stripe-btn="dock"]').click();
    await expect.poll(() => regionWidth(page, "dock")).toBeLessThan(4);

    await page.reload();
    await expect(page.locator(".kbc-codeview")).toBeVisible({ timeout: 15_000 });
    await expect.poll(() => regionWidth(page, "dock")).toBeLessThan(4);
    // Still addressable, still visible as a stripe.
    await expect(page.locator('[data-region="dock"]')).toHaveCount(1);
    await expect(page.locator('[data-desk-stripe-btn="dock"]')).toBeVisible();
  });
});

test.describe("Desk — graduated focus + zoom (V70-A4, the SHOULD set)", () => {
  test.use({ viewport: { width: 1280, height: 720 } });
  test.beforeEach(() => {
    test.skip(!REPO_DIR, "KB_CODE_E2E_REPO_DIR not set — global-setup didn't run");
  });

  test("Ctrl-w m zooms the focused region to fill the shell, and back", async ({ page }) => {
    await openReader(page);
    await page.locator(".cm-content").click();
    const dockBefore = await regionWidth(page, "dock");
    expect(dockBefore).toBeGreaterThan(80);

    await page.keyboard.down("Control");
    await page.keyboard.press("w");
    await page.keyboard.up("Control");
    await page.keyboard.press("m");

    await expect(page.locator("[data-desk-zoom]")).toHaveAttribute("data-desk-zoom", "main");
    // Zoomed: every other region is at its stripe, and main has the room.
    await expect.poll(() => regionWidth(page, "dock")).toBeLessThan(4);
    await expect.poll(() => regionWidth(page, "rail")).toBeLessThan(4);
    // …but never gone: the stripes are still there to get back from.
    await expect(page.locator('[data-region="stripe-left"]')).toBeVisible();

    await page.keyboard.down("Control");
    await page.keyboard.press("w");
    await page.keyboard.up("Control");
    await page.keyboard.press("m");
    await expect(page.locator("[data-desk-zoom]")).toHaveAttribute("data-desk-zoom", "");
    await expect.poll(() => regionWidth(page, "dock")).toBeGreaterThan(80);
  });

  test("F11 is Focus (stripes + a status line remain); Shift-F11 is Present (they do not)", async ({
    page,
  }) => {
    await openReader(page);
    const root = page.locator("[data-desk-chrome]");
    await expect(root).toHaveAttribute("data-desk-chrome", "full");

    await page.keyboard.press("F11");
    await expect(root).toHaveAttribute("data-desk-chrome", "focus");
    await expect.poll(() => regionWidth(page, "dock")).toBeLessThan(4);
    // Graduated, not binary: signposts survive.
    await expect(page.locator('[data-region="stripe-left"]')).toBeVisible();
    await expect(page.locator('[data-region="stripe-right"]')).toBeVisible();
    await expect(page.locator("[data-desk-status]")).toBeVisible();

    await page.keyboard.press("F11");
    await expect(root).toHaveAttribute("data-desk-chrome", "full");

    await page.keyboard.press("Shift+F11");
    await expect(root).toHaveAttribute("data-desk-chrome", "present");
    await expect(page.locator('[data-region="stripe-left"]')).toHaveCount(0);
    await expect(page.locator("[data-desk-status]")).toContainText(/press \? for chrome/i);

    await page.keyboard.press("Shift+F11");
    await expect(root).toHaveAttribute("data-desk-chrome", "full");
    await expect(page.locator('[data-region="stripe-left"]')).toBeVisible();
  });
});

test.describe("Desk — the re-cut rail (V70-A4)", () => {
  test.use({ viewport: { width: 1280, height: 720 } });
  test.beforeEach(() => {
    test.skip(!REPO_DIR, "KB_CODE_E2E_REPO_DIR not set — global-setup didn't run");
  });

  test("tab one is All, and Review is absent without a review for this file", async ({ page }) => {
    await openReader(page, "?desk=read");
    const tabs = page.locator("[data-kbc-itab]");
    await expect(tabs).toHaveCount(4);
    await expect(tabs.nth(0)).toHaveAttribute("data-kbc-itab", "all");
    await expect(tabs.nth(1)).toHaveAttribute("data-kbc-itab", "understand");
    await expect(tabs.nth(2)).toHaveAttribute("data-kbc-itab", "history");
    await expect(tabs.nth(3)).toHaveAttribute("data-kbc-itab", "notes");
    await expect(tabs.nth(0)).toHaveAttribute("aria-selected", "true");
  });

  test("All stacks every section; the other tabs narrow it", async ({ page }) => {
    await openReader(page, "?desk=read");
    const body = page.locator("[data-kbc-rail-body]");
    await expect(body).toHaveAttribute("data-kbc-rail-body", "all");
    for (const s of ["outline", "entity", "provenance", "history", "annotations", "bookmarks"]) {
      await expect(page.locator(`[data-kbc-rail-section="${s}"]`)).toHaveCount(1);
    }

    await page.locator('[data-kbc-itab="notes"]').click();
    await expect(body).toHaveAttribute("data-kbc-rail-body", "notes");
    await expect(page.locator('[data-kbc-rail-section="annotations"]')).toHaveCount(1);
    await expect(page.locator('[data-kbc-rail-section="outline"]')).toHaveCount(0);
  });

  test("the subject chip names the subject on every tab, and the passport cards stay above it", async ({
    page,
  }) => {
    await openReader(page, "?desk=read");
    for (const tab of ["all", "understand", "history", "notes"]) {
      await page.locator(`[data-kbc-itab="${tab}"]`).click();
      // Never blank: the rail always says WHAT it is about.
      await expect(page.locator("[data-kbc-rail-subject]")).toHaveCount(1);
      await expect(page.locator("[data-kbc-rail-subject-name]")).not.toBeEmpty();
      // The three always-visible passport cards live ABOVE the tab body
      // on every tab (root CLAUDE.md invariant #30). They self-suppress
      // when empty, so assert on the ORDER of what does render: the
      // subject chip precedes the body, on every tab.
      const order = await page.evaluate(() => {
        const rail = document.querySelector(".kbc-inspector")!;
        return Array.from(rail.children).map((c) => c.className.split(" ")[0]);
      });
      expect(order.indexOf("kbc-inspector__subject")).toBeLessThan(
        order.indexOf("kbc-inspector__body"),
      );
      expect(order.indexOf("kbc-inspector__icons")).toBeLessThan(
        order.indexOf("kbc-inspector__subject"),
      );
    }
  });

  test("the rail follows the caret, and the pin freezes it while naming where the caret went", async ({
    page,
  }) => {
    await openReader(page, "?desk=read");
    await page.locator(".cm-content").click();

    const name = page.locator("[data-kbc-rail-subject-name]");
    await expect(name).toBeVisible();

    // Land the caret inside a known symbol; the rail follows on a ~250ms
    // debounce.
    await page.keyboard.press("g");
    await page.keyboard.press("g");
    for (let i = 0; i < 40; i++) await page.keyboard.press("j");
    await page.waitForTimeout(500);
    const followed = (await name.textContent())?.trim() ?? "";

    // Pin it, then travel: the header must name BOTH.
    await page.locator('[data-cmd="rail.pin"]').click();
    await expect(page.locator('[data-kbc-rail-pin="1"]')).toHaveCount(1);
    await page.locator(".cm-content").click();
    await page.keyboard.press("g");
    await page.keyboard.press("g");
    await page.waitForTimeout(500);

    const pinned = (await name.textContent())?.trim() ?? "";
    expect(pinned.startsWith("📌"), `pinned header should lead with the pin: ${pinned}`).toBe(true);
    expect(pinned).toContain(followed.replace(/^📌\s*/, ""));

    // Unpin: the rail follows again.
    await page.locator('[data-cmd="rail.pin"]').click();
    await expect(page.locator('[data-kbc-rail-pin="0"]')).toHaveCount(1);
    await expect
      .poll(async () => ((await name.textContent()) ?? "").startsWith("📌"), { timeout: 3_000 })
      .toBe(false);
  });

  test("a Review tab selected with no review context says so, rather than going blank", async ({
    page,
  }) => {
    await openReader(page, "?desk=review");
    await expect(page.locator('[data-kbc-itab="review"]')).toHaveCount(1);
    await expect(page.locator("[data-kbc-rail-no-review]")).toContainText(
      /no review context for this file/i,
    );
  });
});

test.describe("Desk — the drawer (V70-A4)", () => {
  test.use({ viewport: { width: 1280, height: 720 } });
  test.beforeEach(() => {
    test.skip(!REPO_DIR, "KB_CODE_E2E_REPO_DIR not set — global-setup didn't run");
  });

  test("Keep in drawer turns a gr result set into a tab that outlives the popup", async ({ page }) => {
    await openReader(page);
    // Put the caret on a known symbol and ask for its references.
    await page.locator(".cm-content").click();
    await page.getByText(KNOWN_SYMBOL, { exact: false }).first().click();
    await page.keyboard.press("g");
    await page.keyboard.press("r");
    await expect(page.locator("[data-kbc-peek]")).toBeVisible({ timeout: 10_000 });

    const keep = page.locator('[data-cmd="drawer.keep"]');
    await expect(keep).toBeVisible();
    await keep.click();

    // The popup is gone; the rows are not.
    await expect(page.locator("[data-kbc-peek]")).toHaveCount(0);
    const drawer = page.locator('[data-region="drawer"]');
    await expect(drawer).toHaveAttribute("data-desk-drawer-collapsed", "0");
    const tab = page.locator("[data-desk-drawer-tab]").first();
    await expect(tab).toBeVisible();
    await expect(tab).toContainText(KNOWN_SYMBOL);

    // Walkable with j/k, openable with Enter.
    const body = page.locator("[data-desk-drawer-body]");
    await body.click();
    await expect(page.locator("[data-desk-drawer-row].is-cursor")).toHaveCount(1);
    await page.keyboard.press("j");
    await page.keyboard.press("Enter");
    await expect(page.locator(".kbc-codeview")).toBeVisible();

    // Closing a tab is a VIEW operation: it greys, it does not vanish.
    await page.locator('[data-cmd="drawer.close"]').first().click();
    await expect(page.locator('[data-desk-drawer-tab-evicted="1"]')).toHaveCount(1);
    await expect(page.locator('[data-desk-drawer-tab-evicted="1"]')).toContainText(/closed — reopen/i);
  });
});

test.describe("Desk — the legacy shell (V70-A4 / §D1)", () => {
  test.use({ viewport: { width: 1280, height: 720 } });
  test.beforeEach(() => {
    test.skip(!REPO_DIR, "KB_CODE_E2E_REPO_DIR not set — global-setup didn't run");
  });

  test("?shell=legacy still mounts the pre-Desk reader", async ({ page }) => {
    await page.goto(`${READER}?shell=legacy`);
    await expect(page.locator(".kbc-codeview")).toBeVisible({ timeout: 15_000 });
    await expect(page.locator(".kbc-codeview")).toContainText(KNOWN_SYMBOL);
    // The pre-Desk shell: the fixed tree aside, no Desk regions at all.
    await expect(page.locator('[data-region="tree"]')).toHaveCount(1);
    await expect(page.locator("[data-desk-center-mode]")).toHaveCount(0);
    await expect(page.locator('[data-region="stripe-left"]')).toHaveCount(0);
    // The rail is still there, and still the ONE rail (its re-cut tabs
    // are shared — see `routes/ReaderLegacy.tsx`'s header).
    await expect(page.locator(".kbc-inspector")).toHaveCount(1);
  });

  test("?desk=legacy is the same switch", async ({ page }) => {
    await page.goto(`${READER}?desk=legacy`);
    await expect(page.locator(".kbc-codeview")).toBeVisible({ timeout: 15_000 });
    await expect(page.locator('[data-region="tree"]')).toHaveCount(1);
    await expect(page.locator("[data-desk-center-mode]")).toHaveCount(0);
  });
});
