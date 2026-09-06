import { expect, test, type Page } from "@playwright/test";
import { BASE, REPO_DIR, REPO_NAME } from "./helpers";
import { KNOWN_FILE, RUBY_ENTITY, RUBY_ORDER_FILE } from "./fixture-repo";

/// V70-A4 — the LANDMARK golden.
///
/// docs/research/kb-code-v7-continuum-2026-09.html §P1 names it exactly:
/// "a landmark golden (region identity, order and keyboard addressing
/// identical across center modes; **a mode may collapse a region, never
/// move or rename one**)".
///
/// This is the structural promise behind the operator's bar — "travelling
/// continuously between code without hard stops that change the
/// interface". A center mode swaps what MAIN shows; every other region
/// keeps its identity, its position in reading order, and its
/// `data-region` address. When `diff`/`dossier`/`board` land as center
/// modes, they join `MODES` below and this spec runs unchanged — which is
/// why the loop is written now, with one entry in it.
///
/// Deliberately narrower than a full DOM snapshot: it asserts the region
/// SET and its ORDER, never counts, labels or contents. `regions.spec.ts`
/// (the A0 aria gate) covers the landmark ROLES; this one covers the
/// Desk's own addressing.

/// The five regions plus the two stripes, in DOM reading order. This
/// array IS the contract.
const EXPECTED_REGIONS = ["stripe-left", "dock", "main", "drawer", "rail", "stripe-right"] as const;

/// Center modes that a route can mount today. Mirrors
/// `src/desk/centerModes.ts`'s `SHIPPED_CENTER_MODES` by hand — this e2e
/// package is standalone and never imports from `../src` (see
/// `helpers.ts`'s own note on the same discipline for `DeskPreset`).
const MODES = [
  { mode: "reader", url: `${BASE}/r/${REPO_NAME}/${KNOWN_FILE}`, ready: ".kbc-codeview" },
  // V72-G1.2 — the dossier center. D1's rule ("any new surface lands in an
  // existing REGION") is exactly what this loop mechanises: the dossier is a
  // different CENTER over the SAME shell, so it must produce a byte-identical
  // region set. It rides the reader's own route via `?ent=`, which is also
  // the assertion that it did not quietly become a new page.
  {
    mode: "dossier",
    url: `${BASE}/r/${REPO_NAME}/${RUBY_ORDER_FILE}?ent=${encodeURIComponent(RUBY_ENTITY)}`,
    ready: "[data-kbc-dossier]",
  },
] as const;

async function regionOrder(page: Page): Promise<string[]> {
  return page.evaluate(() =>
    Array.from(document.querySelectorAll("[data-region]"))
      .map((el) => el.getAttribute("data-region") ?? "")
      // Desk REGIONS only. The TopBar is app chrome above every route,
      // and `panes`/`pane-1`/`pane-2` are the reader center mode's own
      // inner landmarks — a mode is free to have its own structure
      // inside main, which is exactly what makes it a mode. What the
      // golden pins is the shell AROUND it.
      .filter((r) => r !== "topbar" && r !== "panes" && !r.startsWith("pane-")),
  );
}

test.describe("Desk landmarks (V70-A4)", () => {
  test.use({ viewport: { width: 1280, height: 720 } });

  test.beforeEach(() => {
    test.skip(!REPO_DIR, "KB_CODE_E2E_REPO_DIR not set — global-setup didn't run");
  });

  for (const { mode, url, ready } of MODES) {
    test(`center mode "${mode}" renders every region, in order, exactly once`, async ({ page }) => {
      await page.goto(url);
      await expect(page.locator('[data-region="main"]')).toBeVisible();
      // Each mode names its OWN "the center has painted" selector: the shell
      // is what this golden pins, and a mode is free to fill `main` with
      // whatever it likes (that is what makes it a mode). Waiting on the
      // reader's `.kbc-codeview` for every mode would assert the opposite —
      // that every center is a code buffer — and would have made the dossier
      // fail this spec for being itself.
      await expect(page.locator(ready)).toBeVisible({ timeout: 15_000 });

      const regions = await regionOrder(page);
      expect(regions, `center mode ${mode} moved, renamed or dropped a region`).toEqual([
        ...EXPECTED_REGIONS,
      ]);

      // Addressing is unique: a region name that resolves to two elements
      // is not an address.
      for (const r of EXPECTED_REGIONS) {
        await expect(page.locator(`[data-region="${r}"]`)).toHaveCount(1);
      }

      await expect(page.locator("[data-desk-center-mode]")).toHaveAttribute("data-desk-center-mode", mode);

      // …and the mode's own inner landmarks live INSIDE main, never
      // beside it: a new surface lands in an existing region (§D1).
      const insideMain = await page.evaluate(() => {
        const main = document.querySelector('[data-region="main"]')!;
        return Array.from(document.querySelectorAll('[data-region^="pane-"], [data-region="panes"]')).every(
          (el) => main.contains(el),
        );
      });
      expect(insideMain, `center mode ${mode} put a landmark outside the main region`).toBe(true);
    });
  }

  test("a collapsed region keeps its address and its stripe — it never vanishes", async ({ page }) => {
    await page.goto(`${BASE}/r/${REPO_NAME}/${KNOWN_FILE}`);
    await expect(page.locator(".kbc-codeview")).toBeVisible({ timeout: 15_000 });

    const dock = page.locator('[data-region="dock"]');
    await expect(dock).toBeVisible();
    const before = await regionOrder(page);

    await page.locator('[data-desk-stripe-btn="dock"]').click();
    // Still addressable, still in the same place in reading order — the
    // region is COLLAPSED (zero width), not removed.
    await expect(dock).toHaveCount(1);
    expect(await regionOrder(page)).toEqual(before);
    await expect
      .poll(async () => (await dock.boundingBox())?.width ?? 0, { timeout: 5_000 })
      .toBeLessThan(4);

    // The stripe is the visible form of the collapsed dock.
    await expect(page.locator('[data-region="stripe-left"]')).toBeVisible();
    await expect(page.locator('[data-desk-stripe-btn="dock"]')).toBeVisible();
  });

  test("the Present preset collapses every region but keeps main addressable", async ({ page }) => {
    await page.goto(`${BASE}/r/${REPO_NAME}/${KNOWN_FILE}?desk=present`);
    await expect(page.locator(".kbc-codeview")).toBeVisible({ timeout: 15_000 });
    await expect(page.locator("[data-desk-preset]")).toHaveAttribute("data-desk-preset", "present");
    await expect(page.locator('[data-region="main"]')).toBeVisible();
    await expect
      .poll(async () => (await page.locator('[data-region="rail"]').boundingBox())?.width ?? 0)
      .toBeLessThan(4);
  });

  test("?desk= applies a preset as a one-shot override", async ({ page }) => {
    await page.goto(`${BASE}/r/${REPO_NAME}/${KNOWN_FILE}?desk=review`);
    await expect(page.locator(".kbc-codeview")).toBeVisible({ timeout: 15_000 });
    await expect(page.locator("[data-desk-preset]")).toHaveAttribute("data-desk-preset", "review");
    // Review lands the rail on its own tab and opens the drawer.
    await expect(page.locator('[data-kbc-itab="review"][aria-selected="true"]')).toBeVisible();
    await expect(page.locator('[data-region="drawer"]')).toHaveAttribute(
      "data-desk-drawer-collapsed",
      "0",
    );
  });
});
