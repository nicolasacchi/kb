import { execFileSync } from "node:child_process";
import { writeFileSync } from "node:fs";
import { join } from "node:path";
import { expect, test, type APIRequestContext, type Page } from "@playwright/test";
import {
  CALLER_FILE,
  FEATURE_BRANCH,
  FEATURE_FILE,
  KNOWN_FILE,
  KNOWN_SYMBOL,
  TEXT_NEEDLE,
} from "./fixture-repo";
import { BASE, REPO_DIR, REPO_NAME } from "./helpers";

/// F5 ("kb-code v2 — The Operable Reader", mobile shell) end to end: the
/// hamburger-driven tree drawer, the inspector's mobile bottom sheet, splits
/// stacking vertically at a phone's width, and — the invariant this whole
/// suite exists to police — that NONE of it leaks into the desktop DOM. Runs
/// against the SAME fixture repo every other e2e spec shares.
///
/// Mobile specs pin the viewport to 390×844 (a common phone size); the
/// desktop-parity spec pins 1280×720 explicitly rather than relying on the
/// config's own "Desktop Chrome" default, so this file's intent reads
/// correctly even if that default ever changes.

const MOBILE_VIEWPORT = { width: 390, height: 844 };
const DESKTOP_VIEWPORT = { width: 1280, height: 720 };

test.describe("Reader mobile shell (390×844)", () => {
  test.use({ viewport: MOBILE_VIEWPORT });

  test("the hamburger opens the tree drawer; selecting a file closes it", async ({ page }) => {
    await page.goto(`${BASE}/r/${REPO_NAME}`);

    const burger = page.locator("[data-kbc-burger]");
    await expect(burger).toBeVisible();
    // Closed by default on a cold mobile load — no drawer mounted yet.
    await expect(page.locator("[data-kbc-drawer]")).toHaveCount(0);

    await burger.click();
    const drawer = page.locator("[data-kbc-drawer]");
    await expect(drawer).toBeVisible();
    await expect(page.locator("[data-kbc-drawer-scrim]")).toBeVisible();

    await drawer.locator(".kbc-tree__row", { hasText: KNOWN_FILE }).click();

    await expect(page).toHaveURL(new RegExp(`/r/${REPO_NAME}/${KNOWN_FILE}(\\?|$)`));
    await expect(page.locator(".kbc-codeview")).toContainText(KNOWN_SYMBOL, { timeout: 10_000 });
    // Selecting a file closed the drawer (it unmounts after its close
    // transition — see MobileDrawer.tsx).
    await expect(page.locator("[data-kbc-drawer]")).toHaveCount(0);
  });

  test("Escape and a scrim tap both dismiss the tree drawer", async ({ page }) => {
    await page.goto(`${BASE}/r/${REPO_NAME}`);

    await page.locator("[data-kbc-burger]").click();
    await expect(page.locator("[data-kbc-drawer]")).toBeVisible();
    await page.keyboard.press("Escape");
    await expect(page.locator("[data-kbc-drawer]")).toHaveCount(0);

    await page.locator("[data-kbc-burger]").click();
    await expect(page.locator("[data-kbc-drawer]")).toBeVisible();
    // The drawer itself only spans `min(86vw, 320px)` of the 390px
    // viewport — click the scrim in the sliver to its right, clear of the
    // drawer panel (a point inside the panel's own bounds would hit ITS
    // markup instead, since it paints on top of the full-bleed scrim there).
    await page.locator("[data-kbc-drawer-scrim]").click({ position: { x: 370, y: 400 } });
    await expect(page.locator("[data-kbc-drawer]")).toHaveCount(0);
  });

  test("the inspector sheet opens with the rail, shows the annotations panel, and a scrim tap dismisses it", async ({
    page,
  }) => {
    await page.goto(`${BASE}/r/${REPO_NAME}/${KNOWN_FILE}`);
    await expect(page.locator(".kbc-codeview")).toContainText(KNOWN_SYMBOL, { timeout: 10_000 });

    const toggle = page.locator("[data-kbc-inspector-toggle]");
    await expect(toggle).toBeVisible();
    await toggle.click();

    const sheet = page.locator("#kbc-reader-sheet");
    await expect(sheet).toBeVisible();
    await expect(page.locator("[data-kbc-sheet-scrim]")).toBeVisible();
    // The full icon rail rides inside the sheet — same tabs as desktop.
    // V70-A4 re-cut the six SOURCE tabs into the design's TASK tabs: All,
    // Understand, History, Notes (Review appears only with an open review
    // for this file, and this fixture has none). V72-J2 added Comments,
    // UNCONDITIONAL like the first four, and V74-L3b added Trail on the same
    // footing (design D17: off is a state the rail renders, not an absence).
    await expect(sheet.locator("[data-kbc-itab]")).toHaveCount(6);

    await sheet.locator('[data-kbc-itab="notes"]').click();
    await expect(sheet.locator(".kbc-annotations")).toBeVisible();

    await page.locator("[data-kbc-sheet-scrim]").click({ position: { x: 5, y: 5 } });
    // Unlike the tree drawer (which unmounts its children on close), the
    // inspector sheet is the SAME `<aside>` desktop renders inline — closing
    // it is a pure CSS state change (`.kbc-reader--sheet-open` removed), not
    // an unmount, so the assertion is visibility, not DOM presence.
    await expect(sheet).toBeHidden();
  });

  test("a ?pane2= URL stacks both panes vertically instead of side by side", async ({ page }) => {
    const pane2Value = encodeURIComponent(`${CALLER_FILE}@:`);
    await page.goto(`${BASE}/r/${REPO_NAME}/${KNOWN_FILE}?pane2=${pane2Value}`);

    const panes = page.locator(".kbc-reader__pane");
    await expect(panes).toHaveCount(2);
    await expect(panes.nth(0)).toContainText(TEXT_NEEDLE, { timeout: 10_000 });
    await expect(panes.nth(1)).toContainText("caller_one", { timeout: 10_000 });

    // Stacked (column), not side-by-side (row) — CSS-only, no logic fork:
    // `Reader.tsx` renders the identical two-pane markup at every viewport.
    await expect(page.locator(".kbc-reader__panes")).toHaveCSS("flex-direction", "column");
  });
});

test.describe("Reader desktop shell stays byte-identical (1280×720)", () => {
  test.use({ viewport: DESKTOP_VIEWPORT });

  test("the inspector aside renders with no dialog role and no sheet chrome", async ({ page }) => {
    await page.goto(`${BASE}/r/${REPO_NAME}/${KNOWN_FILE}`);
    await expect(page.locator(".kbc-codeview")).toContainText(KNOWN_SYMBOL, { timeout: 10_000 });

    // The aside exists, undocked from any mobile machinery — `asSheet` is
    // `false` on desktop (isMobile is always false there), so `InspectorRail`
    // spreads an EMPTY object where the sheet's `role`/`aria-modal`/`id`
    // would go.
    const aside = page.locator(".kbc-reader__outline");
    await expect(aside).toBeVisible();
    const inspector = aside.locator(".kbc-inspector");
    await expect(inspector).toHaveCount(1);
    await expect(inspector).not.toHaveAttribute("role");
    await expect(inspector).not.toHaveAttribute("id", "kbc-reader-sheet");
    await expect(page.locator(".kbc-inspector__sheet-head")).toHaveCount(0);
    await expect(page.locator("#kbc-reader-sheet")).toHaveCount(0);

    // The mobile entry points are mounted (so a resize down to a phone
    // works without a remount) but CSS-hidden at this width.
    await expect(page.locator("[data-kbc-burger]")).toBeHidden();
    await expect(page.locator("[data-kbc-inspector-toggle]")).toBeHidden();

    // A split still renders side by side (row), not stacked.
    await expect(page.locator(".kbc-reader__panes")).toHaveCSS("flex-direction", "row");
  });
});

/// V4.M1 — review cockpit sheet + full-page-diff file drawer + coarse
/// comment pill. Existing Reader cases above stay byte-identical.

async function createReview(request: APIRequestContext, title: string, head = FEATURE_BRANCH): Promise<number> {
  const createRes = await request.post(`${BASE}/api/reviews`, {
    data: { repo: REPO_NAME, head_ref: head, base_ref: "main", title },
  });
  expect(createRes.ok(), `create review: ${createRes.status()} ${await createRes.text()}`).toBeTruthy();
  const created = (await createRes.json()) as { id: number };
  return created.id;
}

async function emulateCoarsePointer(page: Page): Promise<void> {
  const cdp = await page.context().newCDPSession(page);
  await cdp.send("Emulation.setEmulatedMedia", {
    features: [
      { name: "pointer", value: "coarse" },
      { name: "any-pointer", value: "coarse" },
    ],
  });
}

test.describe("Review cockpit sheet (390×844)", () => {
  test.use({ viewport: MOBILE_VIEWPORT });

  test("toggle opens the threads sheet; scrim and Escape dismiss it", async ({ page, request }) => {
    test.skip(!REPO_DIR, "KB_CODE_E2E_REPO_DIR not set — global-setup didn't run");
    const reviewId = await createReview(request, "e2e mobile cockpit sheet");
    await page.goto(`${BASE}/r/${REPO_NAME}/~reviews/${reviewId}`);
    await expect(page.locator("[data-kbc-review-title]")).toContainText("e2e mobile cockpit sheet");

    const toggle = page.locator("[data-kbc-review-sheet-toggle]");
    await expect(toggle).toBeVisible();
    await expect(toggle.locator("[data-kbc-review-sheet-toggle-badge]")).toBeVisible();
    await expect(page.locator("#kbc-review-sheet")).toBeHidden();

    await toggle.click();
    const sheet = page.locator("#kbc-review-sheet");
    await expect(sheet).toBeVisible();
    await expect(sheet).toHaveAttribute("role", "dialog");
    await expect(sheet).toHaveAttribute("aria-modal", "true");
    await expect(sheet.locator("[data-kbc-review-threads]")).toBeVisible();
    await expect(page.locator("[data-kbc-review-sheet-scrim]")).toBeVisible();

    await page.locator("[data-kbc-review-sheet-scrim]").click({ position: { x: 5, y: 5 } });
    await expect(sheet).toBeHidden();

    await toggle.click();
    await expect(sheet).toBeVisible();
    await page.keyboard.press("Escape");
    await expect(sheet).toBeHidden();
  });
});

test.describe("Review cockpit desktop-parity (1280×720)", () => {
  test.use({ viewport: DESKTOP_VIEWPORT });

  test("the side panel stays in-grid with no dialog role and no toggle visible", async ({
    page,
    request,
  }) => {
    test.skip(!REPO_DIR, "KB_CODE_E2E_REPO_DIR not set — global-setup didn't run");
    const reviewId = await createReview(request, "e2e desktop review parity");
    await page.goto(`${BASE}/r/${REPO_NAME}/~reviews/${reviewId}`);
    await expect(page.locator("[data-kbc-review-title]")).toContainText("e2e desktop review parity");
    await expect(page.locator("[data-kbc-review-files]")).toBeVisible({ timeout: 10_000 });

    const aside = page.locator(".kbc-review__side");
    await expect(aside).toBeVisible();
    await expect(aside).not.toHaveAttribute("role");
    await expect(page.locator("#kbc-review-sheet")).toHaveCount(0);
    await expect(page.locator(".kbc-review__sheet-head")).toHaveCount(0);
    await expect(page.locator("[data-kbc-review-sheet-toggle]")).toBeHidden();
    await expect(aside).toHaveCSS("position", "sticky");
    await expect(page.locator(".kbc-review__body")).toHaveCSS("grid-template-columns", /280px/);
  });
});

const RDIFF_MOBILE_FILE = "e2e_rdiff_mobile.rs";
const RDIFF_MOBILE_BRANCH = "e2e-rdiff-mobile";

function git(args: string[]): string {
  return execFileSync("git", ["-C", REPO_DIR, ...args], { encoding: "utf-8" }).trim();
}

function tryGit(args: string[]): void {
  try {
    execFileSync("git", ["-C", REPO_DIR, ...args], { stdio: "ignore" });
  } catch {
    // best-effort cleanup
  }
}

test.describe("Review full-page diff mobile (390×844)", () => {
  test.use({ viewport: MOBILE_VIEWPORT });

  test.afterAll(() => {
    if (!REPO_DIR) return;
    tryGit(["checkout", "main"]);
    tryGit(["branch", "-D", RDIFF_MOBILE_BRANCH]);
  });

  test("files toggle opens the drawer; tapping a row scrolls and closes", async ({ page, request }) => {
    test.skip(!REPO_DIR, "KB_CODE_E2E_REPO_DIR not set — global-setup didn't run");

    tryGit(["checkout", "main"]);
    tryGit(["branch", "-D", RDIFF_MOBILE_BRANCH]);
    git(["checkout", "-q", "-b", RDIFF_MOBILE_BRANCH, FEATURE_BRANCH]);
    writeFileSync(join(REPO_DIR, RDIFF_MOBILE_FILE), "fn e2e_rdiff_mobile() -> i32 { 1 }\n");
    git(["add", RDIFF_MOBILE_FILE]);
    git(["commit", "-q", "-m", "e2e rdiff mobile second file"]);
    git(["checkout", "-q", "main"]);

    const reviewId = await createReview(request, "e2e mobile rdiff files", RDIFF_MOBILE_BRANCH);
    await page.goto(`${BASE}/r/${REPO_NAME}/~reviews/${reviewId}/diff`);
    await expect(page.locator("[data-kbc-rdiff]")).toBeVisible({ timeout: 10_000 });
    await expect(page.locator("[data-kbc-rdiff-file]")).toHaveCount(2, { timeout: 10_000 });

    const toggle = page.locator("[data-kbc-rdiff-files-toggle]");
    await expect(toggle).toBeVisible();
    await expect(page.locator("[data-kbc-rdiff-jump]")).toBeHidden();
    await expect(page.locator("[data-kbc-drawer]")).toHaveCount(0);

    await toggle.click();
    const drawer = page.locator("[data-kbc-drawer]");
    await expect(drawer).toBeVisible();
    const row = drawer.locator(`[data-kbc-rdiff-drawer-file="${RDIFF_MOBILE_FILE}"]`);
    await expect(row).toBeVisible();
    await row.click();

    await expect(page.locator("[data-kbc-drawer]")).toHaveCount(0);
    const section = page.locator(`[data-kbc-rdiff-file="${RDIFF_MOBILE_FILE}"]`);
    await expect(section).toBeVisible();
    const box = await section.boundingBox();
    expect(box, "tapped file section should be in the viewport").toBeTruthy();
    expect(box!.y).toBeLessThan(MOBILE_VIEWPORT.height);
  });
});

test.describe("Review diff coarse-pointer pill", () => {
  test.use({ viewport: MOBILE_VIEWPORT, hasTouch: true, isMobile: true });

  test("tapping a review line shows the pill; tapping the pill opens the composer", async ({
    page,
    request,
  }) => {
    test.skip(!REPO_DIR, "KB_CODE_E2E_REPO_DIR not set — global-setup didn't run");
    await emulateCoarsePointer(page);

    const reviewId = await createReview(request, "e2e mobile diff pill");
    await page.goto(`${BASE}/r/${REPO_NAME}/~reviews/${reviewId}/diff`);
    await expect(page.locator("[data-kbc-rdiff]")).toBeVisible({ timeout: 10_000 });

    const line = page.locator(`[data-kbc-rdiff-file="${FEATURE_FILE}"] .kbc-diff__line`).first();
    await expect(line).toBeVisible({ timeout: 10_000 });
    await line.click();

    const pill = page.locator("[data-kbc-diff-pill]");
    await expect(pill).toBeVisible();
    await expect(line).toHaveClass(/is-picked/);
    await pill.locator('[data-kbc-diff-pill-side="new"]').click();
    await expect(page.locator("[data-kbc-review-composer]")).toBeVisible();
  });
});
