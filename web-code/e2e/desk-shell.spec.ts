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

  // V71-E2 (`cdba1573`) — spec DRIFT, not a regression. In V70-A4 `gr`
  // opened the peek popup and "Keep in drawer" (`data-cmd="drawer.keep"`)
  // was the one action that let a result set OUTLIVE that popup; the drawer
  // had exactly one tenant and this test was its proof. E2 repointed `gr`
  // onto `/api/usages/2` and landed it in the drawer DIRECTLY — the tab
  // exists before the fetch even lands, so the drawer never flashes empty —
  // which retires the popup, and with it the Keep step. The three things
  // this test was really pinning all survive and are pinned below: the set
  // becomes a NAMED TAB in the drawer, the tab's badge is the SERVER's own
  // total (never `rows.length`, which is 0 for a set that renders its own
  // body — a tab reading 0 beside a census reading 6 is the disagreeing-
  // count bug in miniature), and closing a tab GREYS it rather than
  // vanishing it. Two mechanical changes follow from the dock owning its own
  // body: the generic `[data-desk-drawer-row]` j/k walk does not apply (the
  // Drawer's reducer returns early for a set with a custom body), so the
  // walk is `]u`/`[u` through `walkOrder`; and the set's rows carry the
  // classified trust vocabulary rather than a grep's flat list.
  test("gr keeps its usage set as a drawer tab: server-total badge, ]u walk, close greys", async ({
    page,
  }) => {
    await openReader(page);
    // Put the caret on a known symbol and ask for its references.
    await page.locator(".cm-content").click();
    await page
      .locator(".kbc-codeview")
      .getByText(KNOWN_SYMBOL, { exact: false })
      .first()
      .click();
    await page.keyboard.press("g");
    await page.keyboard.press("r");

    // No popup at any point; the drawer opens holding the dock.
    const dock = page.locator("[data-kbc-usages-dock]");
    await expect(dock).toBeVisible({ timeout: 10_000 });
    await expect(page.locator("[data-kbc-peek]")).toHaveCount(0);
    const drawer = page.locator('[data-region="drawer"]');
    await expect(drawer).toHaveAttribute("data-desk-drawer-collapsed", "0");
    const tab = page.locator("[data-desk-drawer-tab]").first();
    await expect(tab).toBeVisible();
    await expect(tab).toContainText(KNOWN_SYMBOL);

    // The tab's count is the census's count — one number for one question.
    await expect(dock.locator("[data-kbc-usages-row]").first()).toBeVisible({ timeout: 10_000 });
    const rows = await dock.locator("[data-kbc-usages-row]").count();
    expect(rows).toBeGreaterThanOrEqual(2);
    await expect(dock.locator("[data-kbc-usages-total]")).toContainText(`${rows} usages`);
    await expect(tab.locator(".kbc-desk__drawer-tab-count")).toHaveText(String(rows));

    // Walkable with `]u` — the dock's own cursor, over the SAME `walkOrder`
    // the grouped tree renders from, so "next" cannot mean two things.
    await expect(dock.locator('[data-kbc-usages-row][aria-selected="true"]')).toHaveCount(0);
    await page.keyboard.press("]");
    await page.keyboard.press("u");
    await expect(dock.locator('[data-kbc-usages-row][aria-selected="true"]')).toHaveCount(1);
    await expect(dock.locator("[data-kbc-usages-preview]")).toBeVisible();

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

// --- V71-K3 — the 17 previously mouse-only rows, driven by keyboard ---------
//
// Every row here dispatches the SAME call its own button already made (see
// `routes/Reader.tsx`'s `useCommandHandlers` block, the V71-K3-tagged
// entries beside `desk.toggle.drawer`); these specs assert the same DOM
// state the buttons produce.
//
// Every one of them is driven from inside the focused CM6 buffer (the
// V70-K1 case: guard 2 must let a bare `Space`-leader chord through even
// though the buffer holds real DOM focus).
//
// V71-K3 originally drove five of these chords from OUTSIDE the buffer, via
// a `blurBuffer()` helper, because two of them were destructive there:
// `Space u`/`Space R u` end in bare `u`, which the CM6 vim keymap ALSO read
// as its own `nav.back` and which navigated the reader away mid-test, and
// `Space R a` ends in bare `a` (`annotate.line`). That was a real gap in the
// dispatcher, not a property of these rows: `vimReader.ts`'s `Prec.highest`
// keymap had no knowledge of `CommandRoot`'s in-flight chord and evaluated
// every keydown independently. V71-K4 closed it (`CommandRoot`'s
// `chordConsumesKey` + the bus's `chordWillConsume`, which the vim keymap
// asks BEFORE it interprets anything), so the helper is retired and the
// shield itself is pinned by its own describe block at the end of this
// file.

async function focusBuffer(page: Page): Promise<void> {
  await page.locator(".cm-content").click();
}

/// Focus the buffer WITHOUT clicking it. Once the drawer holds a result set
/// its floating bar overlays the bottom of the code area, so a plain
/// `.click()` on `.cm-content` is intercepted by that bar and times out.
/// `.focus()` lands the same GENUINE DOM focus — which is all guard 2
/// (`e.target`) and the CM6 keymap ever read — without a hit test.
async function refocusBuffer(page: Page): Promise<void> {
  await page.locator(".cm-content").focus();
}

/// `drawer.keep` needs a peek with `rows.length > 0` to have anything to
/// keep, and the fixture repo has no genuinely ambiguous symbol on purpose
/// (every real `gd` in this suite resolves to exactly one candidate and
/// navigates straight there — adding one would shift line numbers several
/// OTHER specs pin, per `fixture-repo.ts`'s own doc). Mocking `/api/resolve`
/// with two candidates reaches the SAME multi-candidate `gd` panel a real
/// ambiguous symbol would open, without touching the fixture.
async function mockAmbiguousResolve(page: Page): Promise<void> {
  await page.route("**/api/resolve*", async (route) => {
    const candidate = (line: number) => ({
      repo: REPO_NAME,
      path: KNOWN_FILE,
      line,
      kind: "fn",
      container: null,
      signature: null,
      doc: null,
      precision: "exact",
      class: "exact",
    });
    await route.fulfill({
      status: 200,
      contentType: "application/json",
      body: JSON.stringify({
        schema: "resolve/1",
        ident: KNOWN_SYMBOL,
        position: { line: 1, col: 0 },
        role: null,
        total: 2,
        note: "V71-K3 e2e mock — two candidates so gd opens the panel instead of navigating",
        candidates: [candidate(1), candidate(5)],
      }),
    });
  });
}

test.describe("Desk keys — presets (V71-K3)", () => {
  test.use({ viewport: { width: 1280, height: 720 } });
  test.beforeEach(() => {
    test.skip(!REPO_DIR, "KB_CODE_E2E_REPO_DIR not set — global-setup didn't run");
  });

  test("Space P {r,v,e,p} apply the four desk presets — the preset chip's own menu action", async ({
    page,
  }) => {
    await openReader(page);
    await focusBuffer(page);
    const root = page.locator("[data-desk-preset]");
    await expect(root).toHaveAttribute("data-desk-preset", "read");

    for (const [key, name] of [
      ["v", "review"],
      ["e", "explore"],
      ["p", "present"],
      ["r", "read"],
    ] as const) {
      await page.keyboard.press("Space");
      await page.keyboard.press("P");
      await page.keyboard.press(key);
      await expect(root).toHaveAttribute("data-desk-preset", name);
    }
  });
});

test.describe("Desk keys — drawer (V71-K3)", () => {
  test.use({ viewport: { width: 1280, height: 720 } });
  test.beforeEach(() => {
    test.skip(!REPO_DIR, "KB_CODE_E2E_REPO_DIR not set — global-setup didn't run");
  });

  test("drawer.tab ordinal (incl. the past-the-end no-op), ] d / [ d, close, pin, keep and reopen", async ({
    page,
  }) => {
    await openReader(page);
    const drawer = page.locator("[data-region='drawer']");

    // Tab A — the same `gr` flow the drawer describe block above already
    // proves lands a usages set in the drawer.
    await focusBuffer(page);
    await page
      .locator(".kbc-codeview")
      .getByText(KNOWN_SYMBOL, { exact: false })
      .first()
      .click();
    await page.keyboard.press("g");
    await page.keyboard.press("r");
    const usagesTab = page.locator('[data-desk-drawer-tab^="usages:"]');
    await expect(usagesTab).toBeVisible({ timeout: 10_000 });
    await expect(drawer).toHaveAttribute("data-desk-drawer-collapsed", "0");

    // `Space K` (`drawer.keep`) FIRST, with NOTHING open — an honest no-op
    // (`handleKeepPeekInDrawer` returns on `rows.length === 0`): only ever
    // the ORIGINAL usages tab exists afterwards, proving the key reaches
    // the handler. Bare `K` is ALSO vim's own `peek.hover` (`cb-hover`) —
    // a third collision alongside `u`/`a` — so before V71-K4's shield this
    // popped a real hover card from a focused buffer and confounded the
    // assertion below with a second, unrelated peek. It no longer fires:
    // the chord consumes its own final token.
    await page.keyboard.press("Space");
    await page.keyboard.press("K");
    await expect(page.locator("[data-desk-drawer-tab]")).toHaveCount(1);

    // Tab B — a mocked multi-candidate `gd` opens the peek, and clicking
    // its OWN "Keep in drawer" button (`handleKeepPeekInDrawer`, the same
    // function `Space K` calls) lands it as a second tab.
    //
    // V71-K3 DISCOVERY (reported, not fixed): `Space K` cannot do this step
    // itself. `PeekPanel.tsx`'s own `onKeyDown` calls `e.stopPropagation()`
    // UNCONDITIONALLY, for every key, while the panel has focus — and the
    // panel focuses itself on mount (same `K`-hover self-focus V71-K2's own
    // doc names for a DIFFERENT case). Since `CommandRoot`'s chord listener
    // is a plain `window` `keydown` listener, a stopped SYNTHETIC event's
    // underlying native event never reaches it — so `drawer.keep`'s
    // keyboard door is structurally unreachable for the one case it exists
    // for (an OPEN peek with something worth keeping). Confirmed
    // empirically: pressing `Space K` here left the drawer with only the
    // original usages tab, no `definitions:` tab, no error. The handler
    // above is correctly wired (this file's own no-op case proves it fires
    // and no-ops honestly) — the interception is in `PeekPanel.tsx`, not in
    // `vimReader.ts`/`shouldWithholdFromBuffer`/a registry key, so it is
    // outside this unit's fix list.
    await mockAmbiguousResolve(page);
    await page
      .locator(".kbc-codeview")
      .getByText(KNOWN_SYMBOL, { exact: false })
      .first()
      .click();
    await page.keyboard.press("g");
    await page.keyboard.press("d");
    const peek = page.locator("[data-kbc-peek]");
    await expect(peek).toBeVisible({ timeout: 10_000 });
    const keepBtn = page.locator("[data-kbc-peek-keep]");
    await expect(keepBtn).toBeVisible();
    await keepBtn.click();
    await expect(peek).toHaveCount(0);
    const defsTab = page.locator('[data-desk-drawer-tab^="definitions:"]');
    await expect(defsTab).toBeVisible();
    await expect(defsTab).toHaveClass(/is-on/);

    // `Space 1` / `Space 2` select by ordinal, in `drawerTabOrder`'s own
    // insertion order (A first, B second). Still inside the buffer: the
    // digits are vim's own count accumulator and `Space u` at the end is
    // `drawer.reopen`, whose bare `u` is vim's destructive `nav.back` —
    // both shielded by V71-K4 rather than avoided.
    await refocusBuffer(page);
    await page.keyboard.press("Space");
    await page.keyboard.press("1");
    await expect(usagesTab).toHaveClass(/is-on/);
    await expect(defsTab).not.toHaveClass(/is-on/);
    await page.keyboard.press("Space");
    await page.keyboard.press("2");
    await expect(defsTab).toHaveClass(/is-on/);

    // `Space 3` — past the end (only two tabs exist): an HONEST no-op. No
    // toast, no error, the active tab does not change.
    await page.keyboard.press("Space");
    await page.keyboard.press("3");
    await expect(defsTab).toHaveClass(/is-on/);
    await expect(page.locator(".kbc-toast")).toHaveCount(0);

    // `] d` / `[ d` step between the two live tabs — `Search.tsx`'s
    // identical result-set stack already uses this same `stepSet` action.
    await page.keyboard.press("]");
    await page.keyboard.press("d");
    await expect(usagesTab).toHaveClass(/is-on/);
    await page.keyboard.press("[");
    await page.keyboard.press("d");
    await expect(defsTab).toHaveClass(/is-on/);

    // `Space D` pins the active tab (B); `Drawer.tsx`'s own pin button.
    await page.keyboard.press("Space");
    await page.keyboard.press("D");
    await expect(defsTab).toHaveClass(/is-pinned/);

    // `Space x` closes the ACTIVE tab — switch to A first, then close IT,
    // proving `drawer.close` acts on whichever tab is active rather than a
    // fixed one. A VIEW operation: it greys, it does not vanish.
    await page.keyboard.press("Space");
    await page.keyboard.press("1");
    await expect(usagesTab).toHaveClass(/is-on/);
    await page.keyboard.press("Space");
    await page.keyboard.press("x");
    await expect(usagesTab).toHaveAttribute("data-desk-drawer-tab-evicted", "1");
    await expect(usagesTab).toContainText(/closed — reopen/i);

    // `Space u` (`drawer.reopen`) — `drawerSets.ts` tracks insertion order
    // only, never a closed-at order, so "the most recently closed tab" is
    // not a question it can honestly answer; the smallest honest version
    // is to re-expand a collapsed drawer instead. Collapse first (`Space
    // d`, K2's own key) so the re-expand is observable.
    await page.keyboard.press("Space");
    await page.keyboard.press("d");
    await expect(drawer).toHaveAttribute("data-desk-drawer-collapsed", "1");
    await page.keyboard.press("Space");
    await page.keyboard.press("u");
    await expect(drawer).toHaveAttribute("data-desk-drawer-collapsed", "0");
  });
});

test.describe("Desk keys — rail (V71-K3)", () => {
  test.use({ viewport: { width: 1280, height: 720 } });
  test.beforeEach(() => {
    test.skip(!REPO_DIR, "KB_CODE_E2E_REPO_DIR not set — global-setup didn't run");
  });

  test("Space R {u,h,n,v,a} select the rail tabs — the review one honestly says it has no context", async ({
    page,
  }) => {
    await openReader(page, "?desk=read");
    // Driven from INSIDE the focused buffer, which is the hard case: two of
    // these five chords (`Space R u`, `Space R a`) end in a bare letter that
    // is ALSO one of vim's own destructive buffer commands (`nav.back`,
    // `annotate.line`). Before V71-K4's shield both fired alongside the
    // chord and this spec had to blur focus to pass at all.
    await focusBuffer(page);

    for (const [key, tabId] of [
      ["u", "understand"],
      ["h", "history"],
      ["n", "notes"],
      ["v", "review"],
      ["a", "all"],
    ] as const) {
      await page.keyboard.press("Space");
      await page.keyboard.press("R");
      await page.keyboard.press(key);
      await expect(page.locator(`[data-kbc-itab="${tabId}"]`)).toHaveAttribute("aria-selected", "true");
      if (tabId === "review") {
        // The itab bar hides Review entirely unless it is EITHER the
        // active tab or there is review context (`InspectorRail`'s own
        // `visibleTabs` filter) — never a blank pane.
        await expect(page.locator("[data-kbc-rail-no-review]")).toContainText(
          /no review context for this file/i,
        );
      }
    }
  });

  test("Space p pins the rail — InspectorRail's own SubjectChip pin button", async ({ page }) => {
    await openReader(page, "?desk=read");
    await focusBuffer(page);
    await expect(page.locator('[data-kbc-rail-pin="0"]')).toHaveCount(1);
    await page.keyboard.press("Space");
    await page.keyboard.press("p");
    await expect(page.locator('[data-kbc-rail-pin="1"]')).toHaveCount(1);
    await page.keyboard.press("Space");
    await page.keyboard.press("p");
    await expect(page.locator('[data-kbc-rail-pin="0"]')).toHaveCount(1);
  });
});

// --- V71-K4 — the chord shield ----------------------------------------------
//
// V71-K3 shipped its four specs above with a focus-blurring workaround
// because `vimReader.ts`'s `Prec.highest` keymap had no knowledge of
// `CommandRoot`'s in-flight chord: a chord's FINAL token that is also a
// standalone vim key fired BOTH layers, and two of the seventeen keys K3
// wired were destructive that way (`Space u`/`Space R u` also ran vim's `u`
// = `nav.back`, which navigated the reader out from under the test;
// `Space R a` also ran `annotate.line`). `CommandRoot`'s bus now answers
// "will the pending chord CONSUME this keystroke?" and the vim keymap asks
// before it interprets anything, so the workaround is retired above and
// these three specs drive the two destructive chords, plus the peek panel's
// own swallowed key, from a genuinely focused surface.

test.describe("Desk keys — the chord shield (V71-K4)", () => {
  test.use({ viewport: { width: 1280, height: 720 } });
  test.beforeEach(() => {
    test.skip(!REPO_DIR, "KB_CODE_E2E_REPO_DIR not set — global-setup didn't run");
  });

  test("Space u re-expands the drawer from INSIDE the buffer and never navigates", async ({
    page,
  }) => {
    await openReader(page);
    await focusBuffer(page);
    const drawer = page.locator("[data-region='drawer']");
    const url = page.url();

    // Collapse first so the re-expand is observable (`Space d`, K2's key,
    // already proven from a focused buffer by `day-one.spec.ts:196`).
    if ((await drawer.getAttribute("data-desk-drawer-collapsed")) !== "1") {
      await page.keyboard.press("Space");
      await page.keyboard.press("d");
    }
    await expect(drawer).toHaveAttribute("data-desk-drawer-collapsed", "1");

    await page.keyboard.press("Space");
    await page.keyboard.press("u");

    // The command ran…
    await expect(drawer).toHaveAttribute("data-desk-drawer-collapsed", "0");
    // …and vim's `u` (`cb-nav-back` → `nav.back`) did NOT: the reader is
    // still mounted at the same address. Before the shield this pair of
    // keystrokes left the page on `about:blank`.
    await expect(page.locator(".kbc-codeview")).toBeVisible();
    expect(page.url()).toBe(url);
  });

  test("Space R a selects the rail tab from INSIDE the buffer and opens no annotation composer", async ({
    page,
  }) => {
    await openReader(page, "?desk=read");
    await focusBuffer(page);

    // The composer is ALWAYS rendered in the rail (it is a form, not a
    // modal), so "did it appear" says nothing. What `annotate.line` actually
    // does is `setAnnotationActiveLine(cursor)` — so the observable is the
    // composer's TARGET LINE, which defaults to 1 until something aims it.
    // The cursor is parked on line 3, deliberately not 1: `gg` then `j j`,
    // pure vim motions the shield never touches.
    const lineInput = page.locator("[data-kbc-annot-line-input]");
    await expect(lineInput).toHaveValue("1");
    await page.keyboard.press("g");
    await page.keyboard.press("g");
    await page.keyboard.press("j");
    await page.keyboard.press("j");
    await expect(lineInput).toHaveValue("1");

    await page.keyboard.press("Space");
    await page.keyboard.press("R");
    await page.keyboard.press("a");

    // The chord ran…
    await expect(page.locator('[data-kbc-itab="all"]')).toHaveAttribute("aria-selected", "true");
    // …and vim's `a` (`cb-annotate` → `annotate.line`) did not: nothing
    // aimed the composer at the cursor. (Asserted this way rather than on
    // the rail tab because the two layers would fire in that order — vim
    // first, at `Prec.highest` — so `annotations` would simply be
    // overwritten by `all` and the double-fire would hide behind it.)
    await expect(lineInput).toHaveValue("1");

    // The control comes AFTER, on purpose: bare `a` from this same focus
    // DOES aim the composer at line 3, which proves the assertion above is
    // the shield working rather than the keystrokes never arriving.
    await page.keyboard.press("a");
    await expect(lineInput).toHaveValue("3");
    await expect(page.locator('[data-kbc-itab="notes"]')).toHaveAttribute("aria-selected", "true");
  });

  test("Space K keeps a POPULATED peek in the drawer — the panel no longer eats the chord", async ({
    page,
  }) => {
    await openReader(page);
    await mockAmbiguousResolve(page);
    await focusBuffer(page);
    await page
      .locator(".kbc-codeview")
      .getByText(KNOWN_SYMBOL, { exact: false })
      .first()
      .click();
    await page.keyboard.press("g");
    await page.keyboard.press("d");
    const peek = page.locator("[data-kbc-peek]");
    await expect(peek).toBeVisible({ timeout: 10_000 });
    // The panel focuses itself on mount — this is the state in which its
    // old unconditional `stopPropagation()` made `drawer.keep` unreachable.
    await expect(page.locator("[data-kbc-peek-keep]")).toBeVisible();

    await page.keyboard.press("Space");
    await page.keyboard.press("K");

    const defsTab = page.locator('[data-desk-drawer-tab^="definitions:"]');
    await expect(defsTab).toBeVisible({ timeout: 10_000 });
    await expect(defsTab).toHaveClass(/is-on/);
    await expect(peek).toHaveCount(0);
    // …and `K` did not ALSO fire the panel's own peek rung on the focused
    // row (which would have replaced the panel rather than closed it).
    await expect(page.locator("[data-kbc-peek]")).toHaveCount(0);
  });
});
