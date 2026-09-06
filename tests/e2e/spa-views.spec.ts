import { test, expect } from "@playwright/test";
import { PORT } from "./helpers";

test.describe("gallery views", () => {
  function port(): number {
    return PORT;
  }

  test("Grid renders cards for each canon artifact", async ({ page }) => {
    await page.goto(`http://127.0.0.1:${port()}/`);
    await expect(page.getByRole("tab", { name: "grid" })).toHaveAttribute(
      "aria-selected",
      "true",
    );
    // 4 root canon files plus pages from multi-page expansion → ≥ 4.
    // Wait for the async docs fetch to paint at least one card before
    // counting — `.count()` is a non-waiting snapshot.
    const cards = page.getByRole("link", { name: /^Open / });
    await expect(cards.first()).toBeVisible();
    const count = await cards.count();
    expect(count).toBeGreaterThanOrEqual(4);
    await expect(
      page.getByRole("link", { name: /Visualizing the Borrow Checker/ }),
    ).toBeVisible();
  });

  test("List view renders rows", async ({ page }) => {
    await page.goto(`http://127.0.0.1:${port()}/?view=list`);
    await expect(
      // exact — the RL-track "Lists" tab is a substring match otherwise
      page.getByRole("tab", { name: "List", exact: true }),
    ).toHaveAttribute(
      "aria-selected",
      "true",
    );
    const rows = page.getByRole("link", { name: /^Open / });
    await expect(rows.first()).toBeVisible();
    const count = await rows.count();
    expect(count).toBeGreaterThanOrEqual(4);
  });

  test("Atlas view renders a canvas with one dot per artifact", async ({
    page,
  }) => {
    // S6 (S-milestone): renderer ported from SVG to Canvas2D, so the
    // dot count is now read off the `data-atlas-count` data attribute
    // instead of a DOM `<circle>` enumeration. Detailed canvas
    // semantics live in spa-atlas.spec.ts.
    await page.goto(`http://127.0.0.1:${port()}/?view=atlas`);
    await expect(page.getByRole("tab", { name: "atlas" })).toHaveAttribute(
      "aria-selected",
      "true",
    );
    const canvas = page.locator(".atlas__canvas");
    await expect(canvas).toBeVisible();
    const count = await canvas.getAttribute("data-atlas-count");
    expect(Number(count ?? "0")).toBeGreaterThanOrEqual(4);
    // Note text varies by atlas-data state (umap-derived /
    // placeholder-positions (W1.F's honest fallback wording) /
    // not-yet-recomputed). Any form is fine here; the dedicated
    // spa-atlas.spec.ts pins the exact wording.
    await expect(
      page.locator(".atlas__note"),
    ).toContainText(
      /(umap-derived layout|hash-placement layout|showing placeholder positions|no atlas data yet)/,
    );
  });

  test("clicking the kb selector keeps view query intact", async ({ page }) => {
    await page.goto(`http://127.0.0.1:${port()}/?view=list`);
    // v0.6 gallery refresh replaced the <select> with a button that
    // opens a listbox popover. Open it, then pick the "canon" option.
    await page.locator(".kb-selector-wrap .kb-ws").click();
    await page.getByRole("option", { name: /canon/ }).click();
    await expect(page).toHaveURL(/view=list/);
    await expect(page).toHaveURL(/kb=canon/);
  });

  test("kb selector on Detail reflects the path kb and navigates to gallery on change", async ({
    page,
    request,
  }) => {
    // Bug fix (plan reviw-of-the-top-majestic-manatee): pre-fix, TopBar
    // read activeKb only from ?kb=, so /a/canon/<file> left the selector
    // pointed at whatever ?kb= was last set to (often stale) and picking
    // a kb on Detail only flipped ?kb= without navigating — the user
    // saw no change. Now: display tracks the path segment; picking a kb
    // off-gallery navigates to /?kb=<name>.
    const r = await request.get(
      `http://127.0.0.1:${port()}/api/kb/canon/docs?limit=1`,
    );
    const rel = (await r.json())[0].source_relative as string;

    await page.goto(`http://127.0.0.1:${port()}/a/canon/${rel}`);
    // Selector text mirrors the path kb (NOT the first kb in the list,
    // which alphabetically would be "canon" anyway here — the assertion
    // pins the rule, not the value).
    await expect(page.locator(".kb-ws-name")).toHaveText("canon");

    // Picking a kb off-gallery navigates to /?kb=<name>.
    await page.locator(".kb-selector-wrap .kb-ws").click();
    await page.getByRole("option", { name: /canon/ }).click();
    await expect(page).toHaveURL(/^[^?]*\/\?kb=canon$/);
  });

  test("sort=title orders cards alphabetically and the select reflects it", async ({
    page,
  }) => {
    await page.goto(`http://127.0.0.1:${port()}/?sort=title`);
    await expect(page.locator(".sort-control__select")).toHaveValue("title");
    await expect(
      page.getByRole("link", { name: /^Open / }).first(),
    ).toBeVisible();
    const labels = await page
      .getByRole("link", { name: /^Open / })
      .evaluateAll((els) =>
        els.map((e) => e.getAttribute("aria-label") || ""),
      );
    expect(labels.length).toBeGreaterThanOrEqual(4);
    // SortControl's "title" key uses localeCompare (lib/sort.ts) — match
    // it here rather than the default code-unit sort.
    const sorted = [...labels].sort((a, b) => a.localeCompare(b));
    expect(labels).toEqual(sorted);
  });

  test("changing the sort dropdown updates the URL and reorders cards", async ({
    page,
  }) => {
    // Regression guard: selecting a sort key via the dropdown (not a
    // deep link) must land in the URL. Previously two back-to-back
    // useUrl `set` calls (sort + dir) read the same stale params
    // snapshot and the dir write clobbered the sort write, so the
    // dropdown did nothing.
    await page.goto(`http://127.0.0.1:${port()}/`);
    await expect(
      page.getByRole("link", { name: /^Open / }).first(),
    ).toBeVisible();

    await page.locator(".sort-control__select").selectOption("title");
    // The sort key must reach the URL — this is what was clobbered.
    await expect(page).toHaveURL(/sort=title/);
    await expect(page.locator(".sort-control__select")).toHaveValue("title");

    // Dropdown resets direction to the key's default (title → asc), so
    // the order matches the `?sort=title` deep link: A→Z by localeCompare.
    // Poll — the reorder lands after the debounced server refetch, not
    // synchronously with the URL change.
    await expect
      .poll(async () => {
        const labels = await page
          .getByRole("link", { name: /^Open / })
          .evaluateAll((els) =>
            els.map((e) => e.getAttribute("aria-label") || ""),
          );
        if (labels.length < 4) return false;
        const sorted = [...labels].sort((a, b) => a.localeCompare(b));
        return JSON.stringify(labels) === JSON.stringify(sorted);
      })
      .toBe(true);
  });

  test("folder filter narrows the gallery to the pm/ artifact set", async ({
    page,
  }) => {
    await page.goto(`http://127.0.0.1:${port()}/?folder=pm`);
    await expect(page.locator(".gallery-h1-accent")).toHaveText("pm");
    // All four direct pm/ artifacts share the "INC-0315 ·" title prefix.
    await expect(
      page.getByRole("link", { name: /Open INC-0315 · Summary/ }),
    ).toBeVisible();
    // 4 direct pm/ artifacts + 1 nested pm/extra/note.html (the gallery
    // folder filter uses prefix-match, so subfolder files are included).
    // The nested file is the G4 fixture seed in global-setup.ts.
    const count = await page
      .getByRole("link", { name: /^Open / })
      .count();
    expect(count).toBe(5);
    // A canon root artifact must be filtered out.
    await expect(
      page.getByRole("link", { name: /Visualizing the Borrow Checker/ }),
    ).toHaveCount(0);
  });

  test("group=folder renders per-folder sections", async ({ page }) => {
    await page.goto(`http://127.0.0.1:${port()}/?group=folder`);
    await expect(page.locator(".gallery-sections")).toBeVisible();
    const names = await page
      .locator(".gallery-section__name")
      .allTextContents();
    expect(names).toContain("pm");
    // Flat canon files bucket into the synthetic "(root)" section.
    expect(names).toContain("(root)");
  });
});
