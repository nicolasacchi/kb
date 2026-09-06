import { test, expect } from "@playwright/test";
import { PORT } from "./helpers";

// In-folder navigation between indexed artifacts. The canon `pm/`
// fixture has four HTML files, ALL indexed individually (the default
// walker behaviour). Clicking a link from one to another in the same
// folder used to mis-route: the iframe probe would fire `pm:page` and
// the SPA appended `?p=<sibling>` to the *current* artifact's URL —
// the cohabiting-siblings bug fixed by the artifact-serve trampoline
// hoist. Now the server short-circuits with `open-artifact` so the
// parent SPA navigates to the sibling's own permalink + subdomain.
//
// To exercise true multi-page (single indexed artifact + sibling
// sub-pages that are NOT indexed individually), a fixture would need
// `skip_patterns` carving the chapters out of the index — out of
// scope for this spec.
test.describe("in-folder cohabiting artifacts", () => {
  const BASE = `http://127.0.0.1:${PORT}`;

  test("internal link to a sibling indexed artifact navigates to its own permalink + subdomain", async ({
    page,
    request,
  }) => {
    // Locate the entrypoint artifact via the docs API. Title matches
    // the <title> tag of corpus/canon/pm/00-summary.html.
    const r = await request.get(`${BASE}/api/kb/canon/docs?limit=200`);
    expect(r.status()).toBe(200);
    const docs = (await r.json()) as Array<{
      title: string;
      source_relative: string;
    }>;
    const summary = docs.find((d) => d.title === "INC-0315 · Summary");
    expect(summary, "INC-0315 · Summary must be indexed").toBeTruthy();
    const rel = summary!.source_relative; // pm/00-summary.html

    await page.goto(`${BASE}/a/canon/${rel}`);
    await expect(
      page.getByRole("navigation", { name: "artifact context" }),
    ).toBeVisible();

    // Click the "next →" link inside the iframe → 01-timeline.html.
    // The artifact server detects the target is itself indexed and
    // returns a trampoline that postMessages `open-artifact`.
    const frame = page.frameLocator(".detail__frame");
    await expect(frame.locator("h1").first()).toBeVisible();
    await frame.getByRole("link", { name: /next/i }).click();

    // Outer URL flips to the sibling's permalink (no `?p=`).
    await expect(page).toHaveURL(
      new RegExp(`/a/canon/pm/01-timeline\\.html(\\?|$)`),
    );

    // The iframe re-mounts on the sibling — its <h1> appears.
    await expect(
      page
        .frameLocator(".detail__frame")
        .getByRole("heading", { name: /Minute-by-minute/i }),
    ).toBeVisible();
  });
});
