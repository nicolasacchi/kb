import { test, expect } from "@playwright/test";
import { PORT } from "./helpers";

// Regression for the iframe back-button trap (A1). Clicking a link from one
// indexed artifact to another used to make the iframe do a real PUSH
// navigation to the server trampoline, polluting the browser's joint
// session history — pressing Back then skipped past the previous artifact
// (often all the way to the gallery). The runtime now intercepts eligible
// same-origin link clicks and uses `location.replace`, so each artifact is
// exactly one browser-history entry and Back returns to where the user came
// from. Uses the canon `pm/` fixture (four individually-indexed,
// cross-linked HTML pages), same as spa-multipage.spec.ts.
test.describe("artifact back-button", () => {
  const BASE = `http://127.0.0.1:${PORT}`;

  // invariant:20
  test("Back returns to the previous artifact, then to the gallery", async ({
    page,
    request,
  }) => {
    const r = await request.get(`${BASE}/api/kb/canon/docs?limit=200`);
    expect(r.status()).toBe(200);
    const docs = (await r.json()) as Array<{
      title: string;
      source_relative: string;
    }>;
    const summary = docs.find((d) => d.title === "INC-0315 · Summary");
    expect(summary, "INC-0315 · Summary must be indexed").toBeTruthy();
    const rel = summary!.source_relative; // pm/00-summary.html

    // gallery → summary: two real history entries.
    await page.goto(`${BASE}/`);
    await page.goto(`${BASE}/a/canon/${rel}`);
    await expect(
      page.getByRole("navigation", { name: "artifact context" }),
    ).toBeVisible();

    // Click the "next →" link inside the iframe → 01-timeline.html. The
    // server returns a trampoline that postMessages `open-artifact`; with
    // the fix the iframe REPLACES (not pushes) on its way there.
    const frame = page.frameLocator(".detail__frame");
    await expect(frame.locator("h1").first()).toBeVisible();
    await frame.getByRole("link", { name: /next/i }).click();

    await expect(page).toHaveURL(
      new RegExp(`/a/canon/pm/01-timeline\\.html(\\?|$)`),
    );
    await expect(
      page
        .frameLocator(".detail__frame")
        .getByRole("heading", { name: /Minute-by-minute/i }),
    ).toBeVisible();

    // THE FIX: one Back press returns to the entry artifact — not a
    // trampoline interstitial, not the gallery.
    await page.goBack();
    await expect(page).toHaveURL(
      new RegExp(`/a/canon/pm/00-summary\\.html(\\?|$)`),
    );
    await expect(
      page.frameLocator(".detail__frame").locator("h1").first(),
    ).toBeVisible();

    // A second Back press reaches the gallery (each artifact = one entry).
    await page.goBack();
    await expect(page).toHaveURL(new RegExp(`127\\.0\\.0\\.1:${PORT}/(\\?|#|$)`));
  });
});
