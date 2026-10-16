import { test, expect } from "@playwright/test";
import { BASE } from "./helpers";

// Regression (plan do-a-big-review-staged-wave): the top buttons + sitewide
// links dropped the active kb when navigating away from a reader in a
// NON-first kb — every link fell back to kbs[0] (the first corpus). The
// reader keeps its kb in the URL PATH (/a/:kb/*) while the section views read
// ?kb=, and the nav builders only saw the query string, so on a reader the
// path kb was invisible to them. A single source of truth (useExplicitKb)
// now carries the path kb into every destination.
//
// Here `mem` is the SECOND configured kb (kbs[0] is `canon`), so a regression
// would visibly snap these links/pill back to `canon`.
test.describe("kb-space continuity across the top nav", () => {
  const DOORS = ["Search", "Memory", "Worklog", "Lists"];

  // invariant:33
  test("reading an artifact in the second kb keeps that kb on every top link", async ({
    page,
  }) => {
    // pref-tabs.html is seeded into the `mem` corpus by global-setup.
    await page.goto(`${BASE}/a/mem/pref-tabs.html`);

    // The workspace pill mirrors the path kb, not the first corpus.
    await expect(page.locator(".kb-ws-name")).toHaveText("mem");

    // Standalone section links carry ?kb=mem …
    const doors = page.getByRole("navigation", { name: "Primary" });
    for (const name of DOORS) {
      await expect(doors.getByRole("link", { name })).toHaveAttribute(
        "href",
        /[?&]kb=mem(&|$)/,
      );
    }
    // … and the Library door (the gallery root) too.
    await expect(doors.getByRole("link", { name: "Library" })).toHaveAttribute(
      "href",
      /^\/\?kb=mem$/,
    );

    // Following a link actually lands on the kb-scoped route — not canon.
    await page
      .getByRole("navigation", { name: "Primary" })
      .getByRole("link", { name: "Search" })
      .click();
    await expect(page).toHaveURL(/\/search\?kb=mem$/);
    await expect(page.locator(".kb-ws-name")).toHaveText("mem");
  });

  test("switching the workspace pill on a section view re-scopes it in place", async ({
    page,
  }) => {
    // Pre-fix this jumped to the gallery from any non-gallery view; now it
    // stays on /search and only swaps the kb. (Switching off a READER still
    // leaves to the gallery — covered in spa-views.spec.ts — because the
    // open artifact belongs to the old kb.)
    await page.goto(`${BASE}/search?kb=mem`);
    await expect(page.locator(".kb-ws-name")).toHaveText("mem");

    await page.locator(".kb-selector-wrap .kb-ws").click();
    await page.getByRole("option", { name: /canon/ }).click();
    await expect(page).toHaveURL(/\/search\?kb=canon$/);
    await expect(page.locator(".kb-ws-name")).toHaveText("canon");
  });
});
