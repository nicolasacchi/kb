import { expect, test } from "@playwright/test";
import { FEATURE_BRANCH } from "./fixture-repo";
import { BASE, REPO_DIR, REPO_NAME } from "./helpers";

/// V75-M3 (D15) — `branch-facts/1`'s views surface. ONE spec, on the
/// existing multi-branch fixture (`main`, `other-branch`, `feature-x`) that
/// `time.spec.ts` and `branches-landing.spec.ts` already drive; it never
/// mutates `main`.
///
/// What it asserts is deliberately the HONESTY surface rather than the
/// layout: that every row carries a CLASSED base, that the eight views are
/// URL-addressable and their counts come off the wire, that the stale rule
/// DEGRADES out loud on a three-branch repo (fewer than the four a
/// percentile can describe), and that a star round-trips. Those are the
/// claims that would be quietly wrong if the wire and the page disagreed;
/// pixel arrangement is not.

test.describe("branch facts — views (V75-M3)", () => {
  test("views are URL-addressable, every row has a classed base, and the rules ride the page", async ({
    page,
  }) => {
    test.skip(!REPO_DIR, "KB_CODE_E2E_REPO_DIR not set — global-setup didn't run");

    await page.goto(`${BASE}/r/${REPO_NAME}/~branches`);

    const views = page.locator("[data-kbc-bviews]");
    await expect(views).toBeVisible({ timeout: 15_000 });
    // The selector is the eight-name closed vocabulary, not a tab bar the
    // page invented.
    await expect(page.locator("[data-kbc-bview]")).toHaveCount(8);
    await expect(views).toHaveAttribute("data-kbc-bviews-view", "all");

    const feature = page.locator(`[data-kbc-bfact="${FEATURE_BRANCH}"]`);
    await expect(feature).toBeVisible();
    // Every row's base is CLASSED — `unknown` is a legal value, a MISSING
    // attribute is not.
    for (const row of await page.locator("[data-kbc-bfact]").all()) {
      const cls = await row.getAttribute("data-kbc-bfact-base");
      expect(["upstream", "fork-point", "merge-base", "unknown"]).toContain(cls);
    }
    // ...and a row with a classed base names the ref it is measured against.
    await expect(feature.locator("[data-kbc-bfact-baseref]")).toContainText("base main");
    // Reason chips are the server's own sentences.
    await expect(feature.locator("[data-kbc-bfact-reason]").first()).toBeVisible();

    // The rules are STATED on the page, not just implemented. Asserting the
    // stale rule's TEXT rather than whether it applied is deliberate: how
    // many branches the fixture has is a property of `fixture-repo.ts` that
    // other lanes extend, and a spec that asserted the DEGRADE would go red
    // the day someone adds a fourth branch for an unrelated reason. What
    // must always hold is that the sentence names what it measured.
    const rules = page.locator("[data-kbc-bviews-rules]");
    await expect(rules.locator("[data-kbc-bviews-stale-rule]")).toContainText("75th percentile");
    await expect(rules.locator("[data-kbc-bviews-stale-rule]")).toContainText("no review on it is open");
    await expect(rules.locator("[data-kbc-bviews-agent-rule]")).toContainText("Co-authored-by");
    await expect(rules.locator("[data-kbc-bviews-counts-note]")).toContainText("ANCESTRY");

    // `active` and `stale` PARTITION the enumeration — the invariant that
    // holds at ANY branch count, read off the selector's own wire-fed
    // counts.
    const countOf = async (v: string) =>
      Number(await page.locator(`[data-kbc-bview-count="${v}"]`).innerText());
    expect((await countOf("active")) + (await countOf("stale"))).toBe(await countOf("all"));

    // A view is a URL. Clicking one writes `?view=`; a reload keeps it.
    await page.locator('[data-kbc-bview="agent"]').click();
    await expect(page).toHaveURL(/\?view=agent/);
    await expect(views).toHaveAttribute("data-kbc-bviews-view", "agent");
    await page.reload();
    await expect(page.locator("[data-kbc-bviews]")).toHaveAttribute(
      "data-kbc-bviews-view",
      "agent",
    );
    // The fixture's commits carry no agent trailer and no configured agent
    // email, so `agent` is honestly EMPTY rather than falling back to all.
    await expect(page.locator("[data-kbc-bviews-empty]")).toBeVisible();

    // Back to `all`, and the count the selector shows is the one the view
    // itself returns.
    await page.locator('[data-kbc-bview="all"]').click();
    const allCount = await page.locator('[data-kbc-bview-count="all"]').innerText();
    await expect(page.locator("[data-kbc-bfact]")).toHaveCount(Number(allCount));
  });

  test("a star round-trips through the daemon and the ★ filter sees it", async ({ page }) => {
    test.skip(!REPO_DIR, "KB_CODE_E2E_REPO_DIR not set — global-setup didn't run");

    await page.goto(`${BASE}/r/${REPO_NAME}/~branches`);
    const star = page.locator(`[data-kbc-bfact-star="${FEATURE_BRANCH}"]`);
    await expect(star).toBeVisible({ timeout: 15_000 });
    await expect(star).toHaveAttribute("aria-pressed", "false");

    await star.click();
    await expect(star).toHaveAttribute("aria-pressed", "true");

    // `?fav=1` is a SERVER filter, not a client one — the round trip is the
    // point.
    await page.locator("[data-kbc-bviews-fav]").click();
    await expect(page).toHaveURL(/fav=1/);
    await expect(page.locator("[data-kbc-bfact]")).toHaveCount(1);
    await expect(page.locator(`[data-kbc-bfact="${FEATURE_BRANCH}"]`)).toBeVisible();

    // Unstar, and the filtered view empties — idempotent both ways.
    await page.locator(`[data-kbc-bfact-star="${FEATURE_BRANCH}"]`).click();
    await expect(page.locator("[data-kbc-bviews-empty]")).toBeVisible();
  });

  test("the conflict radar opens against the default branch and prints its own budget caption", async ({
    page,
  }) => {
    test.skip(!REPO_DIR, "KB_CODE_E2E_REPO_DIR not set — global-setup didn't run");

    await page.goto(`${BASE}/r/${REPO_NAME}/~branches`);
    // Wait for ROWS, not just the section: the radar's target is the
    // default branch off the wire, and the button refuses (stays disabled)
    // until the daemon has said what that is.
    await expect(page.locator("[data-kbc-bfact]").first()).toBeVisible({ timeout: 15_000 });

    await page.locator("[data-kbc-bviews-radar]").click();
    await expect(page).toHaveURL(/radar=main/);

    const radar = page.locator("[data-kbc-radar]");
    await expect(radar).toBeVisible();
    // The caption is the SERVER's, pre-rendered so the CLI prints the same
    // sentence — the page never derives "N of M".
    await expect(radar.locator("[data-kbc-radar-caption]")).toContainText("computed (cap");
    // Each candidate reports clean or conflicting; `feature-x` only adds a
    // file main never touched, so it merges clean.
    const row = radar.locator(`[data-kbc-radar-row="${FEATURE_BRANCH}"]`);
    await expect(row).toBeVisible();
    await expect(row).toHaveAttribute("data-kbc-radar-clean", "1");
  });
});
