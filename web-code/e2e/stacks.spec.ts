import { expect, test } from "@playwright/test";
import {
  FEATURE_BRANCH,
  FEATURE_X2_BRANCH,
  FEATURE_X2_FILE,
} from "./fixture-repo";
import { BASE, REPO_NAME } from "./helpers";

/// V3.3-U1 — stacks surface: 2-layer stack fixture, layer-diff files,
/// empty-state hint when filtering away multi-layer stacks is N/A (we
/// always have the stacked fixture; empty-state is asserted via API all=0
/// only when no multi-layer — with feature-x-2 we expect a stack).

test.describe("stacks page", () => {
  test("stacked fixture shows a 2-layer stack; layer diff is incremental", async ({
    page,
  }) => {
    await page.goto(`${BASE}/r/${REPO_NAME}/~stacks`);
    await expect(page.locator("[data-kbc-stacks]")).toBeVisible({ timeout: 10_000 });

    // Expect a card with feature-x then feature-x-2 (base-first).
    const layerX = page.locator(`[data-kbc-stacks-layer="${FEATURE_BRANCH}"]`);
    const layerX2 = page.locator(`[data-kbc-stacks-layer="${FEATURE_X2_BRANCH}"]`);
    await expect(layerX).toBeVisible({ timeout: 15_000 });
    await expect(layerX2).toBeVisible();

    // Same card contains both layers in base-first order.
    const card = page.locator("[data-kbc-stacks-card]").filter({ has: layerX2 });
    await expect(card).toBeVisible();
    const branchOrder = await card.locator("[data-kbc-stacks-layer]").evaluateAll((els) =>
      els.map((e) => e.getAttribute("data-kbc-stacks-layer")),
    );
    const iX = branchOrder.indexOf(FEATURE_BRANCH);
    const iX2 = branchOrder.indexOf(FEATURE_X2_BRANCH);
    expect(iX).toBeGreaterThanOrEqual(0);
    expect(iX2).toBeGreaterThan(iX);

    // Open feature-x-2 layer diff — only that layer's file (feature_x2.rs).
    await layerX2.click();
    await expect(page.locator("[data-kbc-stacks-diff]")).toBeVisible({ timeout: 10_000 });
    await expect(page.locator("[data-kbc-stacks-diff-head]")).toContainText(/diff vs base/);
    await expect(
      page.locator(`[data-kbc-filechange="${FEATURE_X2_FILE}"]`),
    ).toBeVisible({ timeout: 10_000 });
    // feature-x's own FEATURE_FILE must NOT appear in this incremental diff.
    await expect(page.locator('[data-kbc-filechange="feature_x.rs"]')).toHaveCount(0);
  });

  test("empty state shows the single-layer hint when all is off and no multi-layer", async ({
    page,
  }) => {
    // With the stacked fixture, multi-layer stacks exist — empty state won't
    // show. Assert the empty-state copy via a synthetic check of the EmptyState
    // markup pattern by toggling all=1 first (still has stacks), then document
    // the CLI-mirrored hint strings are present in the component source path:
    // when the list is empty, title + hint render. Drive via route with a
    // repo that has only default — not available here.
    //
    // Instead: confirm the `all` toggle and empty-state data attributes exist
    // in the page shell, and that the hint text is the CLI mirror when we
    // force empty by checking the attribute on EmptyState when stacks API
    // returns []. Use page.route to stub empty stacks once.
    // NOTE the API encoding: fetchStacks sends `all=true` (serde bool),
    // NOT the page-URL's `all=1` convention — the escape hatch must match
    // the request encoding or the toggle path silently stays stubbed.
    await page.route("**/api/stacks?*", async (route) => {
      const url = new URL(route.request().url());
      if (url.searchParams.get("all") === "true") {
        await route.continue();
        return;
      }
      await route.fulfill({
        status: 200,
        contentType: "application/json",
        body: JSON.stringify({
          schema: "stacks/1",
          repo: REPO_NAME,
          default_branch: "main",
          stacks: [],
          truncated: false,
        }),
      });
    });
    await page.goto(`${BASE}/r/${REPO_NAME}/~stacks`);
    await expect(page.locator("[data-kbc-stacks]")).toBeVisible({ timeout: 10_000 });
    await expect(page.getByText("no dependent-branch stacks detected")).toBeVisible();
    // Scope to the empty-state hint — a bare getByText regex also matches
    // the always-visible toggle LABEL (strict-mode violation).
    await expect(page.locator(".kbc-empty__hint")).toContainText(/single-layer/i);

    // Exercise the toggle: all=true escapes the stub to the REAL daemon,
    // whose fixture has the feature-x-2 stack — cards must appear.
    await page.locator("[data-kbc-stacks-all]").check();
    await expect(page.locator("[data-kbc-stacks-card]").first()).toBeVisible({
      timeout: 10_000,
    });
  });
});
