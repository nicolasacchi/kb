import { test, expect } from "@playwright/test";
import { PORT } from "./helpers";

test.describe("gallery views", () => {
  function port(): number {
    return PORT;
  }

  test("Grid renders cards for each canon artifact", async ({ page }) => {
    await page.goto(`http://127.0.0.1:${port()}/`);
    await expect(page.getByRole("navigation", { name: "Primary" }).getByRole("link", { name: "Library" })).toHaveAttribute("aria-current", "page");
    // 4 root canon files plus pages from multi-page expansion → ≥ 4.
    // Wait for the async docs fetch to paint at least one card before
    // counting — `.count()` is a non-waiting snapshot.
    const cards = page.getByRole("link", { name: /^Open / });
    await expect(cards.first()).toBeVisible();
    const count = await cards.count();
    expect(count).toBeGreaterThanOrEqual(4);
    await expect(
      page.locator(".kb-card").getByRole("link", { name: /Visualizing the Borrow Checker/ }),
    ).toBeVisible();
  });

  test("desktop: the touch More button stays hidden and hover still reveals the three actions", async ({ page }) => {
    await page.goto(`http://127.0.0.1:${port()}/`);
    const card = page.locator(".kb-card").first();
    await expect(card).toBeVisible();
    await expect(card.locator(".kb-card__more")).toBeHidden();
    expect(await card.locator(".kb-card__more").evaluate((el) => getComputedStyle(el).display)).toBe("none");
    await card.hover();
    for (const c of [".kb-card__peek", ".kb-card__download", ".kb-card__exclude"]) {
      await expect(card.locator(c)).toBeVisible();
      await expect(card.locator(c)).toHaveCSS("opacity", "1");
    }
  });

  test("the labelled View control reaches every Library view", async ({ page }) => {
    await page.goto(`http://127.0.0.1:${port()}/`);
    const view = page.locator('[data-kb-viewbar-control="view"]');
    await expect(view).toBeVisible();
    await expect(view).toHaveValue("grid");
    for (const [label, value] of [
      ["Index", "list"],
      ["Atlas", "atlas"],
      ["Timeline", "history"],
      ["Canvas", "canvas"],
      ["Front page", "press"],
    ] as const) {
      await view.selectOption({ label });
      await expect(page).toHaveURL(new RegExp(`[?&]view=${value}`));
      await expect(view).toHaveValue(value);
    }
    await view.selectOption({ label: "Shelf" });
    await expect(page).not.toHaveURL(/view=/);
  });

  test("List view renders rows", async ({ page }) => {
    await page.goto(`http://127.0.0.1:${port()}/?view=list`);
    await expect(page.getByRole("navigation", { name: "Primary" }).getByRole("link", { name: "Library" })).toHaveAttribute("aria-current", "page");
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
    await expect(page.getByRole("navigation", { name: "Primary" }).getByRole("link", { name: "Library" })).toHaveAttribute("aria-current", "page");
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

    // Don't touch the dropdown until the app has finished its cold-start
    // URL/state settling: the kb selector resolves from the shared ["kbs"]
    // query (the SSE stream means the page is never network-idle, so wait on
    // that resolved state instead), and a navigation
    // landing after selectOption would overwrite the ?sort= it just wrote
    // (the select then snaps back to its default).
    await expect(page.locator(".kb-ws-name")).toHaveText("canon");

    await page.locator(".sort-control__select").selectOption("title");
    // The sort key must reach the URL — this is what was clobbered.
    await expect(page).toHaveURL(/sort=title/);
    await expect(page.locator(".sort-control__select")).toHaveValue("title");

    // Dropdown resets direction to the key's default (title → asc), so
    // the order matches the `?sort=title` deep link: A→Z by localeCompare.
    // Poll — the reorder lands after the debounced server refetch, not
    // synchronously with the URL change.
    // OL P6 diagnostics: when the order is wrong, name what the server sent
    // for the same query and where each displayed card sits in the DOM.
    const diagnose = async () => {
      const api = await page.request.get(
        `http://127.0.0.1:${port()}/api/kb/canon/docs?sort=title&dir=asc&limit=200`,
      );
      const body = await api.json();
      const docs = (Array.isArray(body) ? body : body.docs) as {
        id: string;
        title: string;
        source_relative: string;
      }[];
      const dom = await page.getByRole("link", { name: /^Open / }).evaluateAll((els) =>
        els.map((e) => {
          const chain: string[] = [];
          for (let n: Element | null = e; n && chain.length < 6; n = n.parentElement) {
            chain.push(n.tagName.toLowerCase() + (n.className ? "." + String(n.className).split(" ")[0] : ""));
          }
          return `${e.getAttribute("aria-label")} @ ${e.getAttribute("href")} <- ${chain.slice(1).join(" < ")}`;
        }),
      );
      return (
        `server: ${JSON.stringify(docs.map((d) => [d.title, d.source_relative]))}\n` +
        `dom:\n${dom.join("\n")}`
      );
    };
    await expect
      .poll(async () => {
        // Shelf cards only: the Front Page band (`.kb-fp`) also holds
        // "Open …" links, in recency order, and shows until the sort leaves
        // "recent".
        const labels = await page
          .getByRole("link", { name: /^Open / })
          .evaluateAll((els) =>
            els
              .filter((e) => !e.closest(".kb-fp"))
              .map((e) => e.getAttribute("aria-label") || ""),
          );
        if (labels.length < 4) return `only ${labels.length} cards`;
        const sorted = [...labels].sort((a, b) => a.localeCompare(b));
        // On failure the received value names the order the page showed.
        return JSON.stringify(labels) === JSON.stringify(sorted)
          ? "sorted"
          : labels.join(" | ") + "\n" + (await diagnose());
      })
      .toBe("sorted");
  });

  test("the unfiltered Library has no header of its own (OL P3)", async ({
    page,
  }) => {
    await page.goto(`http://127.0.0.1:${port()}/`);
    await expect(page.locator(".kb-gallery-viewbar")).toBeVisible();
    await expect(page.locator(".gallery__header")).toHaveCount(0);
  });

  test("folder filter narrows the gallery to the pm/ artifact set", async ({
    page,
  }) => {
    await page.goto(`http://127.0.0.1:${port()}/?folder=pm`);
    await expect(page.locator(".gallery-h1-accent")).toHaveText("pm");
    // OL P3 — one header: the breadcrumb names the folder, so the ViewBar
    // shows no `folder` chip, and the Notes toggle lives in the ViewBar.
    await expect(page.locator(".kb-gallery-viewbar .kb-fchip")).toHaveCount(0);
    await expect(
      page.locator(".kb-gallery-viewbar .gallery-notes-toggle"),
    ).toBeVisible();
    // The selected folder row keeps a readable label (tinted, not solid).
    const onRow = page.locator(".rail-folder.is-on");
    await expect(onRow.locator(".rail-folder-name")).toContainText("pm");
    // The old bug painted --accent-soft as both background and label colour.
    const [fg, bg] = await onRow.evaluate((row) => [
      getComputedStyle(row.querySelector(".rail-folder-name")!).color,
      getComputedStyle(row).backgroundColor,
    ]);
    expect(fg).not.toBe(bg);
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
      page.locator(".kb-card").getByRole("link", { name: /Visualizing the Borrow Checker/ }),
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
