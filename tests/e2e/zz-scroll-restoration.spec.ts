import { test, expect } from "@playwright/test";
import { BASE } from "./helpers";

// X1 — invariant #31: scroll restoration is keyed on the FULL gallery URL
// (path + query), the ContextBar "back to recent" button must replay that
// exact URL (not a bare `/?kb=` grid), and restoring a saved offset is a
// bounded per-frame retry. Two independent, passing pins below.
//
// NOT pinned here (a real, separately-discovered defect, not a test gap):
// leaving the gallery via an in-app card click currently persists "0"
// regardless of the real scroll offset. `VirtualGrid`'s ResizeObserver-driven
// `cols`/`rowCount` recompute (confirmed via toggling the unrelated Notes
// rail, no navigation at all) makes `useWindowVirtualizer` clamp via
// `window.scrollTo(0, 0)`; the scroll-restoration hook's own listener treats
// that clamp as "the user's last real position" and overwrites the correct
// value before `persist()` runs on unmount. Filed for a follow-up fix — see
// the phase report; not something a coverage-only pin should paper over with
// a synthetic setup that avoids the real capture path.
test.describe("gallery scroll restoration (X1)", () => {
  // invariant:31
  test("ContextBar 'back to recent' replays the exact originating (filtered) gallery URL", async ({
    page,
  }) => {
    await page.goto(`${BASE}/?kb=canon&folder=pm`);
    const cards = page.getByRole("link", { name: /^Open /});
    await expect(cards.first()).toBeVisible();
    const galleryUrl = await page.evaluate(
      () => location.pathname + location.search,
    );

    await cards.first().click();
    await expect(
      page.getByRole("navigation", { name: "artifact context" }),
    ).toBeVisible();
    await expect(page).not.toHaveURL(galleryUrl);

    // Not the bare `/?kb=canon` grid — the exact filtered URL the reader
    // came from (the sessionStorage scroll slot is keyed on this).
    await page.locator(".kb-ctxbar__back").click();
    await page.waitForURL((u) => u.pathname + u.search === galleryUrl);
  });

  // invariant:31
  test("a saved scroll offset restores when the gallery URL re-mounts (bounded retry)", async ({
    page,
  }) => {
    // Narrow viewport (single-column per gallery.css) so the 9-doc canon
    // corpus has a real, if modest, window-scrollable range.
    await page.setViewportSize({ width: 420, height: 640 });
    await page.goto(`${BASE}/?kb=canon`);
    await expect(page.getByRole("link", { name: /^Open /}).first()).toBeVisible();
    const maxScroll = await page.evaluate(
      () => document.documentElement.scrollHeight - window.innerHeight,
    );
    expect(maxScroll).toBeGreaterThan(10);

    // Seed the exact slot `useScrollRestoration` reads on mount, then load
    // that URL fresh — proving the restore arm (retry-until-laid-out, target
    // offset) independent of how the slot was written.
    await page.evaluate((y) => {
      sessionStorage.setItem("kb:scroll:/?kb=canon", String(y));
    }, maxScroll);
    await page.reload();
    await expect(page.getByRole("link", { name: /^Open /}).first()).toBeVisible();

    await expect
      .poll(() => page.evaluate(() => window.scrollY), { timeout: 5_000 })
      .toBe(maxScroll);
  });
});
