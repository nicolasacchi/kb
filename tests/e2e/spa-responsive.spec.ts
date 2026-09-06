import { test, expect, type Page } from "@playwright/test";
import { BASE } from "./helpers";

// Responsive / mobile-shell coverage (MB-track). The default project runs at
// devices["Desktop Chrome"] (1280×720) — above the 860px breakpoint — so the
// `desktop` block guards against the mobile overhaul regressing the full
// chrome, and the `mobile` block (viewport 390×844, touch) asserts the
// hamburger drawer + compact header + immersive reading actually engage.
//
// Selectors are the SPA's stable chrome classes (matching the other specs):
//   .kb-burger              hamburger (mobile-only)
//   .kb-head > .kb-viewtoggle   desktop 7-tab strip
//   .kb-head > .kb-inbox / .kb-anchor   header pills (hidden ≤860px — W3)
//   .body--with-rail > .kb-side in-route filter rail (desktop-only on mobile)
//   .kb-drawer / .kb-navlist    off-canvas nav+filters drawer
//   [data-testid=drawer-inbox/-anchors]  the pills' mobile homes (drawer rows)
//   .kb-ctxbar__lbl         word labels on detail action buttons (icon-only on mobile)
//   .kb-immersive-toggle    full-screen reading FAB (mobile-only)

const base = () => BASE;

// A canon root artifact that always exists in the e2e corpus (global-setup.ts).
const ARTIFACT = "/a/canon/kitchen-sink.html";

// W3 — the mobile block seeds ONE open comment + ONE corkboard anchor so the
// header-pill overflow condition is DETERMINISTIC: without it, whether the
// InboxPill/AnchorPill render (and blow the fixed-height no-wrap 390px header
// out sideways) depended on which earlier specs happened to leave open
// comments behind — the 4 no-x-overflow assertions below flapped with suite
// composition. Cleaned up in afterAll (resolve + unpin) so later specs see
// the state they saw before this file ran.
const SEED_BODY = "spa-responsive seed — keeps the inbox pill condition live";

async function kitchenSinkId(
  request: import("@playwright/test").APIRequestContext,
): Promise<string> {
  const r = await request.get(`${base()}/api/kb/canon/docs?limit=20`);
  expect(r.status()).toBe(200);
  const docs = (await r.json()) as { id: string; path: string }[];
  const ks = docs.find((d) => d.path.endsWith("kitchen-sink.html"));
  expect(ks, "kitchen-sink.html indexed in the canon corpus").toBeTruthy();
  return ks!.id;
}

async function expectNoHorizontalScroll(page: Page) {
  const overflow = await page.evaluate(
    () => document.documentElement.scrollWidth - window.innerWidth,
  );
  expect(overflow, "page must not scroll horizontally").toBeLessThanOrEqual(1);
}

// ───────────────────────────── desktop (>860px) ─────────────────────────
test.describe("desktop layout", () => {
  test("gallery: full header strip + sidebar, no hamburger, no x-overflow", async ({
    page,
  }) => {
    await page.goto(`${base()}/`);
    await expect(page.locator(".kb-selector-wrap .kb-ws")).toBeVisible();
    await expect(page.locator(".kb-head > .kb-viewtoggle")).toBeVisible();
    await expect(page.locator(".body--with-rail > .kb-side")).toBeVisible();
    await expect(page.locator(".kb-burger")).toBeHidden();
    await expectNoHorizontalScroll(page);
  });

  test("detail: context bar keeps word labels, no immersive FAB", async ({
    page,
  }) => {
    await page.goto(`${base()}${ARTIFACT}`);
    const copyBtn = page.locator('[data-kb-act="copy-link"]');
    await expect(copyBtn).toBeVisible();
    // Desktop keeps the word label beside the icon.
    await expect(copyBtn.locator(".kb-ctxbar__lbl")).toBeVisible();
    await expect(page.locator(".kb-immersive-toggle")).toBeHidden();
  });
});

// ───────────────────────────── mobile (≤860px) ──────────────────────────
test.describe("mobile layout", () => {
  test.use({ viewport: { width: 390, height: 844 }, hasTouch: true });

  let seeded: { artifactId: string; commentId: string } | null = null;

  test.beforeAll(async ({ playwright }) => {
    const request = await playwright.request.newContext();
    const artifactId = await kitchenSinkId(request);
    // One open comment → InboxPill condition (fleet-wide totalOpen ≥ 1).
    const post = await request.post(
      `${base()}/api/kb/canon/review/${artifactId}/comments`,
      {
        data: { body: SEED_BODY, author: "you", anchor: { kind: "file" } },
      },
    );
    expect(post.status()).toBe(201);
    const comment = (await post.json()) as { id: string };
    seeded = { artifactId, commentId: comment.id };
    // One corkboard pin → AnchorPill condition (idempotent).
    const pin = await request.post(
      `${base()}/api/kb/canon/anchors/${artifactId}`,
    );
    expect(pin.ok()).toBe(true);
    await request.dispose();
  });

  test.afterAll(async ({ playwright }) => {
    if (!seeded) return;
    const request = await playwright.request.newContext();
    await request.post(
      `${base()}/api/kb/canon/review/${seeded.artifactId}/comments/${seeded.commentId}/resolve`,
    );
    await request.delete(
      `${base()}/api/kb/canon/anchors/${seeded.artifactId}`,
    );
    await request.dispose();
  });

  test("gallery: compact header, rail hidden, drawer absent, no x-overflow", async ({
    page,
  }) => {
    await page.goto(`${base()}/`);
    await expect(page.locator(".kb-selector-wrap .kb-ws")).toBeVisible();
    await expect(page.locator(".kb-burger")).toBeVisible();
    await expect(page.locator(".kb-head > .kb-viewtoggle")).toBeHidden();
    // W3 — the seeded comment + anchor guarantee both pills RENDER (attached:
    // proves the ["inbox"]/["anchors"] queries resolved with a count) and the
    // ≤860px CSS hides them (hidden: they'd otherwise overflow 390px by ~48px;
    // a bare toBeHidden would pass vacuously on a not-yet-fetched pill).
    await expect(page.locator(".kb-head > .kb-inbox")).toBeAttached();
    await expect(page.locator(".kb-head > .kb-inbox")).toBeHidden();
    await expect(page.locator(".kb-head > .kb-anchor")).toBeAttached();
    await expect(page.locator(".kb-head > .kb-anchor")).toBeHidden();
    await expect(page.locator(".body--with-rail > .kb-side")).toBeHidden();
    await expect(page.locator(".kb-drawer")).toHaveCount(0);
    await expectNoHorizontalScroll(page);
  });

  test("hamburger opens the drawer; a filter tap keeps it open", async ({
    page,
  }) => {
    await page.goto(`${base()}/`);
    await page.locator(".kb-burger").click();

    const drawer = page.locator(".kb-drawer");
    await expect(drawer).toBeVisible();
    await expect(drawer.locator(".kb-navlist__row").first()).toBeVisible();

    // W3 — the hidden header pills' destinations are homed as drawer rows
    // (mobile's ONLY route to /inbox + /anchors: the bottom bar homes just
    // Recent/Search/Memory/Lists). Badges carry the same counts the pills
    // showed — ≥1 thanks to the seeds.
    const inboxRow = drawer.getByTestId("drawer-inbox");
    await expect(inboxRow).toBeVisible();
    await expect(inboxRow.locator(".kb-navlist__badge")).toHaveText(/[1-9]/);
    const anchorsRow = drawer.getByTestId("drawer-anchors");
    await expect(anchorsRow).toBeVisible();
    await expect(anchorsRow.locator(".kb-navlist__badge")).toHaveText(/[1-9]/);

    // Regression guard: toggling a filter changes ?…= but must NOT close the
    // drawer (the close-effect was keyed on loc.search and slammed it shut on
    // every tag tap; now keyed on loc.pathname only).
    await drawer.locator(".kb-side__dates button", { hasText: "7d" }).click();
    await expect(page).toHaveURL(/since=7d/);
    await expect(drawer).toBeVisible();
    // …and the nav list survives the re-render (drawer didn't re-mount empty).
    await expect(drawer.locator(".kb-navlist__row").first()).toBeVisible();
  });

  test("a drawer nav row navigates and closes the drawer", async ({ page }) => {
    await page.goto(`${base()}/`);
    await page.locator(".kb-burger").click();

    const drawer = page.locator(".kb-drawer");
    await expect(drawer).toBeVisible();
    // The destination rows are <Link>s; pick by accessible name.
    await drawer.getByRole("link", { name: "Sessions" }).click();
    await expect(page).toHaveURL(/\/sessions/);
    await expect(drawer).toHaveCount(0);
  });

  test("reading view: icon-only context bar + immersive full-screen toggle", async ({
    page,
  }) => {
    await page.goto(`${base()}${ARTIFACT}`);
    await expect(page.locator(".kb-ctxbar")).toBeVisible();
    await expectNoHorizontalScroll(page);

    // Action labels collapse to icons on a phone (tooltip carries meaning).
    await expect(
      page.locator('[data-kb-act="copy-link"] .kb-ctxbar__lbl'),
    ).toBeHidden();

    // Immersive mode hides all chrome; Esc restores it.
    const toggle = page.locator(".kb-immersive-toggle");
    await expect(toggle).toBeVisible();
    await toggle.click();
    await expect(page.locator("body.kb-immersive")).toHaveCount(1);
    await expect(page.locator(".kb-head")).toBeHidden();

    await page.keyboard.press("Escape");
    await expect(page.locator("body.kb-immersive")).toHaveCount(0);
    await expect(page.locator(".kb-head")).toBeVisible();
  });

  // invariant:30
  test("reading view: ONE reader-tools button raises one sheet; panels switch inside it", async ({
    page,
  }) => {
    await page.goto(`${base()}${ARTIFACT}`);
    await expect(page.locator(".kb-ctxbar")).toBeVisible();

    // v0.23 — exactly ONE reader-verb --mobile toggle survives (reader tools);
    // the standalone comments/versions mobile toggles were removed.
    await expect(page.locator('[data-kb-act="inspect"]')).toBeVisible();
    await expect(page.locator('[data-kb-act="comments"]')).toHaveCount(0);
    await expect(page.locator('[data-kb-act="versions"]')).toHaveCount(0);

    const detail = page.locator(".detail");
    const sheet = page.locator(".kb-pinsp");
    // Closed: the sheet is mounted but inert (translated offscreen + hidden).
    await expect(detail).not.toHaveClass(/detail--inspector-open/);
    await expect(sheet).toBeHidden();

    // Tapping the single button raises the sheet with its full merged rail.
    await page.locator('[data-kb-act="inspect"]').click();
    await expect(detail).toHaveClass(/detail--inspector-open/);
    await expect(sheet).toBeVisible();
    await expect(sheet.locator(".kb-pinsp__icons")).toBeVisible();

    // Regression guard for the pre-v0.23 "railless dead-end": switching to
    // versions from INSIDE the sheet (the always-present dock-versions rail
    // icon) must KEEP the sheet open with its rail — the old mobile CSS
    // dissolved the rail and split the panel into its own railless sheet.
    await sheet.locator('[data-kb-act="dock-versions"]').click();
    await expect(detail).toHaveClass(/detail--inspector-open/);
    await expect(sheet).toBeVisible();
    await expect(sheet.locator(".kb-pinsp__icons")).toBeVisible();
    await expect(sheet.locator(".versions-panel")).toBeVisible();

    // Esc closes the whole sheet.
    await page.keyboard.press("Escape");
    await expect(detail).not.toHaveClass(/detail--inspector-open/);
    await expect(sheet).toBeHidden();

    // Re-open, then dismiss via a scrim tap. The scrim sits UNDER the sheet
    // (--z-scrim < --z-drawer), so its tappable area is the strip ABOVE the
    // sheet — click near the top, not the covered centre.
    await page.locator('[data-kb-act="inspect"]').click();
    await expect(sheet).toBeVisible();
    await page.locator(".kb-pinsp-scrim").click({ position: { x: 195, y: 24 } });
    await expect(detail).not.toHaveClass(/detail--inspector-open/);
    await expect(sheet).toBeHidden();

    await expectNoHorizontalScroll(page);
  });

  test("atlas: renders full-width + touch-enabled on mobile, no x-overflow", async ({
    page,
  }) => {
    await page.goto(`${base()}/?view=atlas`);
    const canvas = page.locator(".atlas__canvas");
    await expect(canvas).toBeAttached();
    // The canvas paints itself (the draw loop only runs with a non-zero box),
    // proving it's laid out + sized on a phone. We assert that via the data
    // attributes it stamps rather than Playwright's strict visibility check,
    // which is finicky on an aspect-ratio-sized <canvas> in headless chromium.
    await expect(canvas).toHaveAttribute("data-atlas-count", /[1-9]/);
    await expect(canvas).toHaveAttribute("data-first-dot", /\d/);
    // It must span the full content width — no sideways scroll on the page.
    const box = await canvas.boundingBox();
    expect(box, "canvas has a layout box").not.toBeNull();
    expect(box!.width).toBeGreaterThan(200);
    await expectNoHorizontalScroll(page);
    // Pointer-event gestures (pan/pinch/tap) need the browser to stop
    // claiming touch for scroll/zoom.
    const touchAction = await canvas.evaluate(
      (el) => getComputedStyle(el).touchAction,
    );
    expect(touchAction).toBe("none");
  });
});
