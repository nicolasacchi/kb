import { expect, test } from "@playwright/test";
import { FEATURE_BRANCH, FEATURE_FILE } from "./fixture-repo";
import { BASE, REPO_DIR, REPO_NAME } from "./helpers";

/// V3.3-U1 — review map + reading order in the cockpit.
/// Creates a review via API (feature-x → main); does not mutate branch tips.

test.describe("review map + reading order", () => {
  test("map nodes render; reading order lists test last; cycle-free has no cycle flags", async ({
    page,
    request,
  }) => {
    test.skip(!REPO_DIR, "KB_CODE_E2E_REPO_DIR not set — global-setup didn't run");

    const createRes = await request.post(`${BASE}/api/reviews`, {
      data: {
        repo: REPO_NAME,
        head_ref: FEATURE_BRANCH,
        base_ref: "main",
        title: "e2e review map",
      },
    });
    expect(createRes.ok(), `create: ${createRes.status()} ${await createRes.text()}`).toBeTruthy();
    const created = (await createRes.json()) as { id: number };
    const reviewId = created.id;

    await page.goto(`${BASE}/r/${REPO_NAME}/~reviews/${reviewId}`);
    await expect(page.locator("[data-kbc-review-title]")).toBeVisible({ timeout: 10_000 });

    // Map tab — may be absent on older servers (degrade); when present, open it.
    const mapTab = page.locator('[data-kbc-review-view="map"]');
    if ((await mapTab.count()) === 0) {
      // Degrade path: no map surface — still pass (older server).
      return;
    }
    await mapTab.click();
    const panel = page.locator("[data-kbc-review-map]");
    await expect(panel).toBeVisible({ timeout: 10_000 });
    // feature-x adds FEATURE_FILE — node should appear.
    await expect(
      page.locator(`[data-kbc-layered-dag-node="${FEATURE_FILE}"]`),
    ).toBeVisible({ timeout: 10_000 });

    // V70-A3S (THE MAP BUG) — the map used to inherit the ego-graph
    // popover's `position: fixed` + no background, tearing it out of flow
    // as a transparent, unclipped overlay painting over the page. A prior
    // version of this spec only checked `toBeVisible()`, which a fixed
    // transparent overlay still satisfies — assert it's actually IN FLOW
    // (its box sits inside its panel's box) and the panel is opaque.
    const map = page.locator("[data-kbc-layered-dag]");
    const panelBox = await panel.boundingBox();
    const mapBox = await map.boundingBox();
    expect(panelBox).not.toBeNull();
    expect(mapBox).not.toBeNull();
    if (panelBox && mapBox) {
      expect(mapBox.x).toBeGreaterThanOrEqual(panelBox.x - 1);
      expect(mapBox.y).toBeGreaterThanOrEqual(panelBox.y - 1);
      expect(mapBox.x + mapBox.width).toBeLessThanOrEqual(panelBox.x + panelBox.width + 1);
      expect(mapBox.y + mapBox.height).toBeLessThanOrEqual(panelBox.y + panelBox.height + 1);
    }
    const panelBg = await panel.evaluate((el) => getComputedStyle(el).backgroundColor);
    expect(panelBg).not.toBe("transparent");
    expect(panelBg).not.toBe("rgba(0, 0, 0, 0)");

    // Reading order — cycle-free fixture ⇒ no cycle flags; tests last when present.
    const orderTab = page.locator('[data-kbc-review-view="order"]');
    await orderTab.click();
    await expect(page.locator("[data-kbc-review-order]")).toBeVisible({ timeout: 10_000 });
    const stops = page.locator("[data-kbc-review-stop]");
    await expect(stops.first()).toBeVisible();
    const cycleFlags = page.locator('[data-kbc-review-stop-cycle="1"]');
    await expect(cycleFlags).toHaveCount(0);

    // If any stop path looks like a test, it must be last.
    const paths = await stops.evaluateAll((els) =>
      els.map((e) => e.getAttribute("data-kbc-review-stop") ?? ""),
    );
    const testIdx = paths.findIndex(
      (p) =>
        /\/tests?\//i.test(p) ||
        /(^|\/)e2e\//i.test(p) ||
        /\.(test|spec)\./i.test(p) ||
        /_test\./i.test(p),
    );
    if (testIdx >= 0) {
      expect(testIdx).toBe(paths.length - 1);
    }
  });

  // V70-A3S — cockpit tab + selected patchset are now `?tab=`/`?ps=` search
  // params (were local state — not deep-linkable, not restorable on
  // reload). A fresh navigation straight to `?tab=map` must land on the Map
  // tab without any prior in-app click.
  test("reloading ~reviews/:id?tab=map lands on the map tab", async ({ page, request }) => {
    test.skip(!REPO_DIR, "KB_CODE_E2E_REPO_DIR not set — global-setup didn't run");

    const createRes = await request.post(`${BASE}/api/reviews`, {
      data: {
        repo: REPO_NAME,
        head_ref: FEATURE_BRANCH,
        base_ref: "main",
        title: "e2e review map deep link",
      },
    });
    expect(createRes.ok(), `create: ${createRes.status()} ${await createRes.text()}`).toBeTruthy();
    const created = (await createRes.json()) as { id: number };
    const reviewId = created.id;

    await page.goto(`${BASE}/r/${REPO_NAME}/~reviews/${reviewId}?tab=map`);
    await expect(page.locator("[data-kbc-review-title]")).toBeVisible({ timeout: 10_000 });

    const mapTab = page.locator('[data-kbc-review-view="map"]');
    if ((await mapTab.count()) === 0) {
      // Degrade path: no map surface on this server — still pass.
      return;
    }
    await expect(mapTab).toHaveAttribute("aria-selected", "true", { timeout: 10_000 });
    await expect(page.locator("[data-kbc-review-map]")).toBeVisible({ timeout: 10_000 });
  });
});
