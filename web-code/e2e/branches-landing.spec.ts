import { expect, test } from "@playwright/test";
import { FEATURE_BRANCH, OTHER_BRANCH } from "./fixture-repo";
import { BASE, REPO_DIR, REPO_NAME } from "./helpers";

/// V4.L2 — smart branch landing + shared ref typeahead. Uses the existing
/// multi-branch fixture (`main`, `other-branch`, `feature-x`) from
/// `fixture-repo.ts` — same repo `time.spec.ts` / `reviews.spec.ts` drive.
/// Does not mutate `main`. The open-review case creates a review via API
/// against `feature-x` (additive; other specs already do this).

test.describe("branch landing (V4.L2)", () => {
  test("hero shows the default branch; ranked list renders reason chips", async ({ page }) => {
    test.skip(!REPO_DIR, "KB_CODE_E2E_REPO_DIR not set — global-setup didn't run");

    await page.goto(`${BASE}/r/${REPO_NAME}/~branches`);

    const hero = page.locator("[data-kbc-branches-hero]");
    await expect(hero).toBeVisible({ timeout: 10_000 });
    await expect(hero.locator("[data-kbc-branches-hero-name]")).toHaveText("main");
    await expect(hero.locator("[data-kbc-branches-hero-default]")).toHaveText("default");

    const ranked = page.locator("[data-kbc-ranked]");
    await expect(ranked).toBeVisible();
    const featureRow = ranked.locator(`[data-kbc-ranked-row="${FEATURE_BRANCH}"]`);
    await expect(featureRow).toBeVisible();
    await expect(featureRow.locator("[data-kbc-reason]").first()).toBeVisible();
    // Fresh fixture commits land inside the recency half-life.
    await expect(featureRow.locator('[data-kbc-reason="recency"]')).toBeVisible();
  });

  test("Start-review CTA opens the dialog preseeded with head + default base", async ({ page }) => {
    test.skip(!REPO_DIR, "KB_CODE_E2E_REPO_DIR not set — global-setup didn't run");

    await page.goto(`${BASE}/r/${REPO_NAME}/~branches`);

    const row = page.locator(`[data-kbc-ranked-row="${OTHER_BRANCH}"]`);
    await expect(row).toBeVisible({ timeout: 10_000 });
    await row.locator(`[data-kbc-ranked-start="${OTHER_BRANCH}"]`).click();

    const dlg = page.locator("[data-kbc-start-review]");
    await expect(dlg).toBeVisible();
    await expect(dlg.locator("[data-kbc-start-review-head]")).toHaveValue(OTHER_BRANCH);
    await expect(dlg.locator("[data-kbc-start-review-base]")).toHaveValue("main");
  });

  test("open-review CTA reads Open review and navigates to the cockpit", async ({
    page,
    request,
  }) => {
    test.skip(!REPO_DIR, "KB_CODE_E2E_REPO_DIR not set — global-setup didn't run");

    const createRes = await request.post(`${BASE}/api/reviews`, {
      data: {
        repo: REPO_NAME,
        head_ref: FEATURE_BRANCH,
        base_ref: "main",
        title: "e2e landing open review",
      },
    });
    expect(createRes.ok(), `create review: ${createRes.status()} ${await createRes.text()}`).toBeTruthy();
    const created = (await createRes.json()) as { id: number };
    expect(created.id).toBeGreaterThan(0);

    await page.goto(`${BASE}/r/${REPO_NAME}/~branches`);
    const row = page.locator(`[data-kbc-ranked-row="${FEATURE_BRANCH}"]`);
    await expect(row).toBeVisible({ timeout: 10_000 });
    const cta = row.locator(`[data-kbc-ranked-open="${FEATURE_BRANCH}"]`);
    await expect(cta).toHaveText("Open review");
    await cta.click();
    await expect(page).toHaveURL(new RegExp(`~reviews/${created.id}`));
  });

  test("browse-all toggles and filter narrows table rows", async ({ page }) => {
    test.skip(!REPO_DIR, "KB_CODE_E2E_REPO_DIR not set — global-setup didn't run");

    await page.goto(`${BASE}/r/${REPO_NAME}/~branches`);

    const browse = page.locator("[data-kbc-browse]");
    await expect(browse).toBeVisible({ timeout: 10_000 });
    await expect(browse.locator("[data-kbc-browse-toggle]")).toContainText("All branches (");

    // Fixture has 3 branches (≤5) so the section starts OPEN per spec.
    // Exercise collapse → expand, then filter.
    // Sibling specs mutate the shared fixture repo (side branches), so the
    // absolute branch count is not ours to assert. Derive N from the
    // toggle's own "All branches (N)" label and require self-consistency.
    const table = browse.locator(".kbc-branches__table");
    if (await table.isHidden()) {
      await browse.locator("[data-kbc-browse-toggle]").click();
    }
    await expect(table).toBeVisible();
    const label = await browse.locator("[data-kbc-browse-toggle]").innerText();
    const n = Number(/All branches \((\d+)\)/.exec(label)?.[1] ?? "0");
    expect(n).toBeGreaterThanOrEqual(3);
    await expect(browse.locator("[data-kbc-branch-row]")).toHaveCount(n);

    await browse.locator("[data-kbc-browse-toggle]").click();
    await expect(table).toBeHidden();

    await browse.locator("[data-kbc-browse-toggle]").click();
    await expect(table).toBeVisible();

    // The filter is speedSearch-FUZZY by design and the fixture repo is
    // shared, so assert the CONTRACT — filtering narrows and keeps the
    // target — never an absolute match count.
    await browse.locator("[data-kbc-browse-filter]").fill(FEATURE_BRANCH);
    await expect(browse.locator(`[data-kbc-branch-row="${FEATURE_BRANCH}"]`)).toBeVisible();
    await expect
      .poll(async () => browse.locator("[data-kbc-branch-row]").count())
      .toBeLessThan(n);
    await expect(browse.locator(`[data-kbc-branch-row="${FEATURE_BRANCH}"]`)).toBeVisible();
  });

  test("Compare typeahead still submits the same from/to query params", async ({ page }) => {
    test.skip(!REPO_DIR, "KB_CODE_E2E_REPO_DIR not set — global-setup didn't run");

    await page.goto(`${BASE}/r/${REPO_NAME}/~compare`);

    const from = page.locator("[data-kbc-compare-from]");
    const to = page.locator("[data-kbc-compare-to]");
    await expect(from).toBeVisible({ timeout: 10_000 });
    await expect(page.locator("[data-kbc-reftypeahead]").first()).toBeVisible();

    await from.fill("main");
    await to.fill(FEATURE_BRANCH);
    await page.locator(".kbc-compare__go").click();
    await expect(page).toHaveURL(new RegExp(`~compare\\?from=main&to=${FEATURE_BRANCH}`));
  });
});
