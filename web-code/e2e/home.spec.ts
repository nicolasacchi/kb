import { expect, test } from "@playwright/test";
import { FEATURE_BRANCH, KNOWN_FILE, KNOWN_SYMBOL } from "./fixture-repo";
import { BASE, REPO_NAME } from "./helpers";

/// F4/F6 — Home's fleet dashboard (one card per configured repo: HEAD chip,
/// Continue reading, Recent files, Active branches, footer links) plus the
/// F6 EmptyState sweep's two most observable surfaces (Home's own no-repos
/// state, and the /search page's no-query state). Runs against the SAME
/// fixture repo every other e2e spec shares (`fixture-repo.ts`) — `main`'s
/// initial commit, `other-branch` (same tip as `main`), and `feature-x`
/// (one commit ahead, adding `feature_x.rs`).
test.describe("Home dashboard", () => {
  test("renders the fixture repo card with a HEAD chip, then Continue reading + Recent files once a file has been opened", async ({
    page,
  }) => {
    // Drive one file open through the tree first — Home's "Continue
    // reading" (the sessionStorage working set) and "Recent files" (the
    // files lane's own empty-query frecency, bumped server-side on every
    // successful `GET /api/file`) both need a real file open to have
    // anything to show; the fixture's other specs may ALSO have opened
    // files earlier in this same daemon run, but KNOWN_FILE is guaranteed
    // to be among them either way once this open completes.
    await page.goto(`${BASE}/r/${REPO_NAME}`);
    await page.locator(".kbc-tree__row", { hasText: KNOWN_FILE }).click();
    await expect(page.locator(".kbc-codeview")).toContainText(KNOWN_SYMBOL, { timeout: 10_000 });

    await page.goto(`${BASE}/`);
    const card = page.locator(`[data-kbc-home-card="${REPO_NAME}"]`);
    await expect(card).toBeVisible();

    // HEAD chip — the fixture's HEAD is on `main` with no `?ref=` pin.
    const headChip = card.locator("[data-kbc-home-head]");
    await expect(headChip).toBeVisible();
    await expect(headChip).toContainText("main");

    // OL-C7 — the per-card "Continue reading" / "Recent files" duplicates
    // are gone; the file just opened leads Home's own Continue section.
    await expect(card.locator("[data-kbc-home-continue]")).toHaveCount(0);
    await expect(card.locator("[data-kbc-home-recent]")).toHaveCount(0);
    await expect(card.locator("[data-kbc-home-store]")).toHaveCount(0);
    const hero = page.locator("[data-kbc-home-continue-hero]");
    await expect(hero).toBeVisible({ timeout: 10_000 });
    await expect(hero).toContainText(KNOWN_FILE);
    // Real code under the location: the file's first lines, highlighted.
    await expect(hero.locator("[data-kbc-hl-snippet]")).toBeVisible({ timeout: 10_000 });
  });

  test("Active branches lists feature-x with an ahead chip that links into a prefilled compare", async ({ page }) => {
    await page.goto(`${BASE}/`);
    const card = page.locator(`[data-kbc-home-card="${REPO_NAME}"]`);
    await expect(card).toBeVisible();

    const featureRow = card.locator(`[data-kbc-home-branch-row="${FEATURE_BRANCH}"]`);
    await expect(featureRow).toBeVisible({ timeout: 10_000 });
    await expect(featureRow.locator("[data-kbc-home-branch-ahead]")).toContainText("1");

    await featureRow.locator("[data-kbc-home-branch-ahead] a").click();
    await expect(page).toHaveURL(new RegExp(`~compare\\?from=main&to=${FEATURE_BRANCH}`));
  });
});

// V70-A3S (design doc §Decisions D23) — the two repo-LESS lanes above the
// repo grid: the "One Inbox" summary (reuses `useUnifiedInbox`, same
// endpoint `~inbox`'s own page hits) and the cross-repo "Continue where you
// left off" card (`lib/navHistory.ts`'s browser-local location ring, via
// `lib/continueLocations.ts` — a SEPARATE data source from RepoCard's own
// per-repo "Continue reading," which reads the sessionStorage working set).
test.describe("Home — One Inbox summary + Continue card (V70-A3S)", () => {
  test("renders both sections; Continue lists the file just opened", async ({ page }) => {
    // Seed the navHistory ring the same way the "Home dashboard" describe
    // above does — open a real file through the tree first.
    await page.goto(`${BASE}/r/${REPO_NAME}`);
    await page.locator(".kbc-tree__row", { hasText: KNOWN_FILE }).click();
    await expect(page.locator(".kbc-codeview")).toContainText(KNOWN_SYMBOL, { timeout: 10_000 });

    await page.goto(`${BASE}/`);

    // Needs-you section — always renders. OL-C7: only non-empty lanes show;
    // an all-zero inbox is ONE quiet line, never four.
    const inboxSummary = page.locator("[data-kbc-home-inbox-summary]");
    await expect(inboxSummary).toBeVisible({ timeout: 10_000 });
    await expect(
      inboxSummary.locator("[data-kbc-home-inbox-lane], [data-kbc-home-inbox-empty]").first(),
    ).toBeVisible({ timeout: 10_000 });
    await expect(inboxSummary.locator("[data-kbc-home-inbox-empty]")).toHaveCount(
      (await inboxSummary.locator("[data-kbc-home-inbox-lane]").count()) === 0 ? 1 : 0,
    );

    // Continue card — the file just opened is in the ring.
    const continueCard = page.locator("[data-kbc-home-continue-global]");
    await expect(continueCard).toBeVisible({ timeout: 10_000 });
    await expect(continueCard).toContainText(KNOWN_FILE);
    await expect(continueCard).toContainText(REPO_NAME);
  });
});

test.describe("Home — Daemon status (OL-C8)", () => {
  test("Home keeps one Daemon status link to Settings, with no diagnostics inline", async ({ page }) => {
    await page.goto(`${BASE}/`);
    const status = page.locator("[data-kbc-home-status]");
    await expect(status).toBeVisible();
    await expect(page.locator("[data-kbc-home-store]")).toHaveCount(0);
    await status.locator("a").click();
    await expect(page).toHaveURL(/\/~settings$/);
    await expect(page.locator("[data-kbc-home-store]").first()).toBeVisible({ timeout: 10_000 });
  });
});

test.describe("F6 EmptyState sweep", () => {
  test("Home shows no EmptyState when a repo is configured; /search shows one before any query is typed", async ({
    page,
  }) => {
    await page.goto(`${BASE}/`);
    await expect(page.locator(`[data-kbc-home-card="${REPO_NAME}"]`)).toBeVisible();
    // The fixture always has one repo configured, so Home's "no repos
    // configured" EmptyState must never render here.
    await expect(page.locator("[data-kbc-empty]")).toHaveCount(0);

    await page.goto(`${BASE}/search`);
    const empty = page.locator("[data-kbc-empty]");
    await expect(empty).toBeVisible();
    await expect(empty).toContainText("Search everywhere");
  });
});
