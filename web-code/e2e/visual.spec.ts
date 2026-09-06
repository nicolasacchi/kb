import { expect, test } from "@playwright/test";
import { buildRegionRoutes } from "./regions-routes";
import { REPO_DIR } from "./helpers";

/// V70-A0 — the visual regression PASS for kb-code v7's Track A (the Desk
/// shell decomposition of `Reader.tsx`, docs/research/
/// kb-code-v7-continuum-2026-09.html §D1: "a visual-regression baseline
/// over every route lands before the refactor"). Deliberately opt-in
/// (`KBC_VISUAL=1`) rather than part of the default `npx playwright test`
/// run: full-page screenshots are the slowest and most machine-sensitive
/// kind of check this suite has (font rasterization, GPU compositing,
/// scrollbar rendering all drift by platform/driver in ways `regions.spec
/// .ts`'s ARIA snapshots never do), so they are NOT part of `just
/// ci-code-e2e` / CI. The milestone's own "before/after screenshot review"
/// (the same recon-cited lesson from v0.37's rail-over-content regression,
/// docs/research/kb-code-v7-evidence/recon/layout-rails-panels.md §7) is a
/// human-driven pass over these baselines, run locally on this box.
///
/// Regenerate baselines after an intentional visual change:
///   cd web-code/e2e && KBC_VISUAL=1 npx playwright test visual --update-snapshots
/// See this directory's README.md for the full recipe.
///
/// Shares `regions-routes.ts`'s route list with `regions.spec.ts` — one
/// route table, never two that can drift apart.

const VIEWPORT = { width: 1280, height: 720 };

test.describe("visual baselines (V70-A0, opt-in via KBC_VISUAL=1)", () => {
  test.use({ viewport: VIEWPORT });

  test("every reachable client route matches its committed screenshot", async ({ page, request }) => {
    test.skip(process.env.KBC_VISUAL !== "1", "opt-in — set KBC_VISUAL=1 to run the visual pass");
    test.skip(!REPO_DIR, "KB_CODE_E2E_REPO_DIR not set — global-setup didn't run");

    const { routes, cleanup } = await buildRegionRoutes(request);
    const skipped: string[] = [];
    let taken = 0;

    try {
      for (const route of routes) {
        if (route.skip) {
          skipped.push(`${route.name}: ${route.skip}`);
          continue;
        }
        await test.step(route.name, async () => {
          await page.goto(route.url);
          // Same settle signal `regions.spec.ts` uses — the one element
          // every route renders (the global TopBar), so a route that's
          // still loading its OWN content doesn't get screenshotted mid-
          // skeleton. Full-page capture still includes below-the-fold
          // content once it's painted; `toHaveScreenshot` itself polls
          // until the render is stable.
          await expect(page.locator('[data-region="topbar"]')).toBeVisible();
          await expect(page).toHaveScreenshot(`${route.name}.png`, {
            fullPage: true,
            animations: "disabled",
            maxDiffPixelRatio: 0.02,
          });
          taken++;
        });
      }
    } finally {
      // See `regions-routes.ts`'s header doc — this daemon is shared with
      // the rest of the suite when run without a `--grep`/project filter.
      await cleanup();
    }

    console.log(
      `visual.spec.ts: ${taken} screenshot(s) taken, ${skipped.length} route(s) skipped — ${
        skipped.join(" | ") || "(none)"
      }`,
    );
  });
});
