import { expect, test } from "@playwright/test";
import { KNOWN_FILE, KNOWN_SYMBOL } from "./fixture-repo";
import { BASE, REPO_NAME } from "./helpers";

/// W4.4's blame gutter, driven against the fixture repo — whose single
/// commit carries no `Kb-Session` trailer and was made with `[kb_daemon]
/// enabled = false` (`global-setup.ts`'s doc), so the join ladder can NEVER
/// resolve a session for it. This spec exercises exactly the HONEST-ABSENCE
/// path (W4.4 step 4): every line's attribution comes back
/// `confidence: "none"`, so the gutter renders NO dots at all (step 1's
/// gate), yet every line is still clickable — the why-panel opens and
/// says so plainly, rather than the reader silently pretending provenance
/// doesn't exist as a feature.
test.describe("blame gutter — disclosure ladder", () => {
  test("shows no dots for a file with zero join hits, and honest absence on click", async ({ page }) => {
    // A HARD navigation straight to `/r/<repo>/<file>.rs` 404s server-side:
    // `spa::serve`'s asset-vs-shell split treats any request path with a
    // dot-extension as a built asset to read off disk
    // (`spa.rs::has_extension`), not a client route to hand `index.html` —
    // true for kb-server's OWN artifact paths too, but THOSE never carry a
    // source-file extension. A reader deep-link to an actual source file
    // does, and has no asset at that path, so it 404s instead of falling
    // back to the SPA shell. Not this milestone's server surface to touch
    // (see the build brief) — load the extensionless repo ROOT (a real hard
    // navigation, safe) and reach the file via CLIENT-SIDE navigation (the
    // file tree), same pattern `omnibox-search.spec.ts` already uses.
    await page.goto(`${BASE}/r/${REPO_NAME}`);
    await page.locator(".kbc-tree__row", { hasText: KNOWN_FILE }).click();

    // The file loaded (CM6 rendered the fixture's known symbol).
    await expect(page.locator(".kbc-codeview")).toContainText(KNOWN_SYMBOL, { timeout: 10_000 });

    const toggle = page.locator("[data-kbc-provenance-toggle]");
    await expect(toggle).toBeVisible();
    // Set up both waits BEFORE clicking: the toggle fires `GET /api/blame`,
    // and once it resolves `useBlameAttributions` immediately fires the
    // lazy per-sha `GET /api/why?line=` prefetch (ONE call — the fixture's
    // single commit means every region shares one sha) — waiting for both
    // up front avoids a race against either firing before this test starts
    // listening.
    const blamePromise = page.waitForResponse((res) => res.url().includes("/api/blame?"));
    const whyPrefetchPromise = page.waitForResponse((res) => res.url().includes("/api/why?"));
    await toggle.click();
    await blamePromise;
    await whyPrefetchPromise;

    // Step 1's gate ("join confidence != none") — a repo the local kb
    // daemon was never asked about (disabled) resolves EVERY line to
    // `confidence: "none"`, so the gutter renders not one dot.
    await expect(page.locator(".kbc-blame-dot--solid, .kbc-blame-dot--outline")).toHaveCount(0);

    // Every line is still clickable (honest absence, not a dead gutter) —
    // click line 2's row by Y-coordinate delegation through the gutter's
    // own container (there's no marker element to click there at all).
    const lineTwo = page.locator(".cm-lineNumbers .cm-gutterElement", { hasText: /^2$/ }).first();
    const lineBox = await lineTwo.boundingBox();
    const blameGutter = page.locator(".cm-gutter.kbc-blame-gutter");
    const gutterBox = await blameGutter.boundingBox();
    expect(lineBox).not.toBeNull();
    expect(gutterBox).not.toBeNull();
    // No NEW network call is expected here — the click just opens the
    // panel from the attribution already cached by the prefetch above.
    await page.mouse.click(gutterBox!.x + gutterBox!.width / 2, lineBox!.y + lineBox!.height / 2);

    const panel = page.locator('[data-kbc-why-line="2"]');
    await expect(panel).toBeVisible({ timeout: 10_000 });
    await expect(panel.locator("[data-kbc-why-confidence]")).toHaveAttribute("data-kbc-why-confidence", "none");
    // Line 2 is a COMMITTED line (the fixture makes one clean commit, no
    // dirty edits) — the "no recorded session" arm, not "uncommitted".
    await expect(panel.locator('[data-kbc-why-absence="no-session"]')).toBeVisible();
    await expect(panel).toContainText("no recorded session");
  });
});
