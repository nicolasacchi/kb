import { test, expect, type Page } from "@playwright/test";
import { BASE } from "./helpers";

// Responsive / mobile-shell coverage (MB-track). The default project runs at
// devices["Desktop Chrome"] (1280×720) — above the 860px breakpoint — so the
// `desktop` block guards against the mobile overhaul regressing the full
// chrome, and the `mobile` block (viewport 390×844, touch) asserts the
// hamburger drawer + compact header + immersive reading actually engage.
//
// Selectors are the SPA's stable chrome classes (matching the other specs):
//   [data-kb-spine-menu]    Spine menu button (mobile-only)
//   .kb-head .kb-doors   desktop view strip (Spine switcher slot)
//   .kb-head .kb-inbox / .kb-anchor   header pills (Spine right slot; hidden ≤860px — W3)
//   .body--with-rail > .kb-side in-route filter rail (desktop-only on mobile)
//   [data-kb-sheet] / .kb-navlist    nav+filters Sheet (OL-L1)
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
  const { overflow, wide } = await page.evaluate(() => {
    const vw = window.innerWidth;
    const wide = Array.from(document.body.querySelectorAll<HTMLElement>("*"))
      .filter((el) => el.getBoundingClientRect().right > vw + 1)
      .slice(0, 6)
      .map((el) => `${el.tagName.toLowerCase()}.${String(el.className).slice(0, 40)}`);
    return { overflow: document.documentElement.scrollWidth - vw, wide };
  });
  expect(overflow, `page must not scroll horizontally (wide: ${wide.join(", ")})`).toBeLessThanOrEqual(1);
}

// ───────────────────────────── desktop (>860px) ─────────────────────────
test.describe("desktop layout", () => {
  test("gallery: full header strip + sidebar, no hamburger, no x-overflow", async ({
    page,
  }) => {
    await page.goto(`${base()}/`);
    await expect(page.locator(".kb-selector-wrap .kb-ws")).toBeVisible();
    await expect(page.locator(".kb-head .kb-doors")).toBeVisible();
    await expect(page.locator(".body--with-rail > .kb-side")).toBeVisible();
    await expect(page.locator("[data-kb-spine-menu]")).toBeHidden();
    await expectNoHorizontalScroll(page);
  });

  test("detail: context bar keeps word labels, no immersive FAB", async ({
    page,
  }) => {
    await page.goto(`${base()}${ARTIFACT}`);
    const shareBtn = page.locator('[data-kb-act="share"]');
    await expect(shareBtn).toBeVisible();
    // Desktop keeps the word label beside the icon.
    await expect(shareBtn.locator(".kb-ctxbar__lbl")).toBeVisible();
    // The secondary verbs live in More, labelled and keyed.
    await page.locator('[data-kb-act="more"]').click();
    await expect(page.locator('[data-kb-act="copy-link"]')).toBeVisible();
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
    await expect(page.locator("[data-kb-spine-menu]")).toBeVisible();
    await expect(page.locator(".kb-head .kb-doors")).toBeHidden();
    // W3 — the seeded comment + anchor guarantee both pills RENDER (attached:
    // proves the ["inbox"]/["anchors"] queries resolved with a count) and the
    // ≤860px CSS hides them (hidden: they'd otherwise overflow 390px by ~48px;
    // a bare toBeHidden would pass vacuously on a not-yet-fetched pill).
    await expect(page.locator(".kb-head .kb-inbox")).toBeAttached();
    await expect(page.locator(".kb-head .kb-inbox")).toBeHidden();
    await expect(page.locator(".kb-head .kb-anchor")).toBeAttached();
    await expect(page.locator(".kb-head .kb-anchor")).toBeHidden();
    await expect(page.locator(".body--with-rail > .kb-side")).toBeHidden();
    await expect(page.locator("[data-kb-sheet]")).toHaveCount(0);
    await expectNoHorizontalScroll(page);
  });

  test("hamburger opens the drawer; a filter tap keeps it open", async ({
    page,
  }) => {
    await page.goto(`${base()}/`);
    await page.locator("[data-kb-spine-menu]").click();

    const drawer = page.locator("[data-kb-sheet]");
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
    await page.locator("[data-kb-spine-menu]").click();

    const drawer = page.locator("[data-kb-sheet]");
    await expect(drawer).toBeVisible();
    // The destination rows are <Link>s; pick by accessible name.
    await drawer.getByRole("link", { name: "Worklog" }).click();
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
      page.locator('[data-kb-act="share"] .kb-ctxbar__lbl'),
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

// ───────────── OL L6: the phone pass on the Library landing ─────────────
test.describe("mobile library landing (OL L6)", () => {
  test.use({ viewport: { width: 390, height: 844 }, hasTouch: true });

  test("Front Page is one column; lead at the title step; coverage below", async ({ page }) => {
    await page.goto(`${base()}/?kb=canon`);
    const band = page.locator('[data-testid="frontpage"]');
    await expect(band).toBeVisible();
    await expect(page.locator(".kb-card").first()).toBeVisible();
    await expectNoHorizontalScroll(page);

    const cols = await page.locator(".kb-fp__page").evaluate(
      (el) => getComputedStyle(el).gridTemplateColumns.trim().split(/\s+/).length,
    );
    expect(cols).toBe(1);

    const lead = page.locator(".kb-fp__lead-title");
    await expect(lead).toHaveCount(1);
    const [leadPx, titlePx] = await lead.evaluate((el) => [
      parseFloat(getComputedStyle(el).fontSize),
      parseFloat(getComputedStyle(document.documentElement).getPropertyValue("--fs-title")),
    ]);
    expect(leadPx).toBe(titlePx);

    const main = await page.locator(".kb-fp__main").boundingBox();
    const cov = await page.locator(".kb-fp__coverage").boundingBox();
    if (main && cov) expect(cov.y).toBeGreaterThanOrEqual(main.y + main.height - 1);
  });

  test("ViewBar controls and Front Page tools are >=44px; the bar wraps at most two rows", async ({ page }) => {
    await page.goto(`${base()}/?kb=canon`);
    await expect(page.locator("[data-kb-viewbar]").first()).toBeVisible();
    const targets = [
      page.locator("[data-kb-viewbar] .kb-viewbar__btn"),
      page.locator("[data-kb-viewbar] .kb-viewbar__select"),
      page.locator(".kb-fp__tool"),
      page.locator("[data-kb-spine-menu]"),
      page.locator(".kb-head .kb-ws"),
      page.locator(".kb-head .kb-search-mini"),
    ];
    let seen = 0;
    for (const loc of targets) {
      for (const el of await loc.all()) {
        if (!(await el.isVisible())) continue;
        const box = await el.boundingBox();
        expect(box?.height ?? 0).toBeGreaterThanOrEqual(43.5);
        seen++;
      }
    }
    expect(seen).toBeGreaterThan(2);

    const rows = await page.locator("[data-kb-viewbar]").first().evaluate((bar) => {
      const tops = new Set<number>();
      for (const c of Array.from(bar.querySelectorAll<HTMLElement>(".kb-viewbar__lead, .kb-viewbar__clusters"))) {
        tops.add(Math.round(c.getBoundingClientRect().top));
      }
      return tops.size;
    });
    expect(rows).toBeLessThanOrEqual(2);
  });

  test("card quick actions never cover the card title", async ({ page }) => {
    await page.goto(`${base()}/?kb=canon&folder=pm&sort=title&dir=asc`);
    const card = page.locator(".kb-card").first();
    await expect(card).toBeVisible();
    const title = await card.locator(".kb-card__title").boundingBox();
    expect(title).not.toBeNull();
    let n = 0;
    for (const a of await card.locator(".kb-card__action").all()) {
      const b = await a.boundingBox();
      if (!b || !title) continue;
      n++;
      const overlap =
        b.x < title.x + title.width && b.x + b.width > title.x &&
        b.y < title.y + title.height && b.y + b.height > title.y;
      expect(overlap).toBe(false);
      expect(b.height).toBeGreaterThanOrEqual(43.5);
    }
    expect(n).toBeGreaterThan(0);
  });

  test("touch shows exactly one 44px More button that clears the title and progress bar", async ({ page }) => {
    await page.goto(`${base()}/?kb=canon&folder=pm&sort=title&dir=asc`);
    const card = page.locator(".kb-card").first();
    await expect(card).toBeVisible();
    const vis = card.locator(".kb-card__action:visible");
    await expect(vis).toHaveCount(1);
    await expect(vis).toHaveAttribute("data-kb-act", "more");
    const b = (await vis.boundingBox())!;
    expect(b.width).toBeGreaterThanOrEqual(43.5);
    expect(b.height).toBeGreaterThanOrEqual(43.5);
    const title = (await card.locator(".kb-card__title").boundingBox())!;
    expect(
      b.x < title.x + title.width && b.x + b.width > title.x &&
      b.y < title.y + title.height && b.y + b.height > title.y,
    ).toBe(false);
    const card_ = (await card.boundingBox())!;
    expect(b.y + b.height).toBeLessThanOrEqual(card_.y + card_.height - 2 + 0.5);
  });

  test("More opens the actions sheet; Esc closes and refocuses More", async ({ page }) => {
    await page.goto(`${base()}/?kb=canon&folder=pm&sort=title&dir=asc`);
    const card = page.locator(".kb-card").first();
    const more = card.locator('[data-kb-act="more"]');
    const url = page.url();
    await more.tap();
    const dlg = page.getByRole("dialog");
    await expect(dlg).toBeVisible();
    for (const n of ["Preview", "Download", "Exclude from index"]) {
      await expect(dlg.getByRole("button", { name: n, exact: true })).toBeVisible();
    }
    expect(page.url()).toBe(url);
    await page.keyboard.press("Escape");
    await expect(dlg).toBeHidden();
    await expect(more).toBeFocused();
  });

  test("More -> Exclude closes the sheet and asks to confirm; cancelling keeps the card", async ({ page }) => {
    await page.goto(`${base()}/?kb=canon&folder=pm&sort=title&dir=asc`);
    const cards = page.locator(".kb-card");
    const n = await cards.count();
    const more = cards.first().locator('[data-kb-act="more"]');
    await more.tap();
    await page.getByRole("dialog").getByRole("button", { name: "Exclude from index", exact: true }).tap();
    await expect(page.locator(".confirm__go")).toBeVisible();
    await expect(page.locator("[data-kb-sheet]")).toHaveCount(0);
    await page.keyboard.press("Escape");
    await expect(page.locator(".confirm__go")).toBeHidden();
    expect(await cards.count()).toBe(n);
    await expect(more).toBeFocused();
  });

  test("More -> Download fires a download", async ({ page }) => {
    await page.goto(`${base()}/?kb=canon&folder=pm&sort=title&dir=asc`);
    await page.locator(".kb-card").first().locator('[data-kb-act="more"]').tap();
    const dl = page.waitForEvent("download");
    await page.getByRole("dialog").getByRole("button", { name: "Download", exact: true }).tap();
    await dl;
    await expect(page.getByRole("dialog")).toBeHidden();
  });

  test("Spine search stays on one line; menu glyph is centred; bottom bar names the Library", async ({ page }) => {
    await page.goto(`${base()}/?kb=canon`);
    const ph = page.locator(".kb-head .kb-search-mini__placeholder");
    await expect(ph).toBeVisible();
    const lh = await ph.evaluate((el) => parseFloat(getComputedStyle(el).lineHeight) || 16);
    expect((await ph.boundingBox())!.height).toBeLessThan(lh * 1.6);
    const menu = page.locator("[data-kb-spine-menu]");
    const mb = (await menu.boundingBox())!;
    const glyph = await menu.evaluate((el) => {
      const r = document.createRange();
      r.selectNodeContents(el);
      const g = r.getBoundingClientRect();
      return { top: g.top, bottom: g.bottom };
    });
    const mid = (glyph.top + glyph.bottom) / 2;
    expect(Math.abs(mid - (mb.y + mb.height / 2))).toBeLessThan(4);
    await expect(page.locator(".kb-tabbar .kb-tabbar__tab").first()).toHaveText("Library");
  });

  test("reader strip at 390: crumb and position label give way, More inside the viewport", async ({ page }) => {
    await page.goto(`${base()}${ARTIFACT}`);
    const more = page.locator('[data-kb-act="more"]');
    await expect(more).toBeVisible();
    const b = (await more.boundingBox())!;
    expect(b.x + b.width).toBeLessThanOrEqual(390.5);
    await expect(page.locator(".kb-ctxbar__crumb")).toBeHidden();
    await expect(page.locator(".kb-ctxbar__pos")).toBeHidden();
    await expect(page.locator('[data-kb-act="inspect"]')).toBeAttached();
  });

  test("reader strip at 600: crumb visible, >=48px, ellipsized; position label on one line", async ({ page }) => {
    await page.setViewportSize({ width: 600, height: 844 });
    await page.goto(`${base()}${ARTIFACT}`);
    const crumb = page.locator(".kb-ctxbar__crumb");
    await expect(crumb).toBeVisible();
    expect((await crumb.boundingBox())!.width).toBeGreaterThanOrEqual(47.5);
    const css = await crumb.evaluate((el) => {
      const c = getComputedStyle(el);
      return [c.overflow, c.textOverflow, c.whiteSpace];
    });
    expect(css).toEqual(["hidden", "ellipsis", "nowrap"]);
    const pos = page.locator(".kb-ctxbar__pos");
    await expect(pos).toBeVisible();
    const lh = await pos.evaluate((el) => parseFloat(getComputedStyle(el).lineHeight) || 14);
    expect((await pos.boundingBox())!.height).toBeLessThan(lh * 1.6);
  });
});
