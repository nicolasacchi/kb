import { expect, test, type Page } from "@playwright/test";
import { BASE, REPO_NAME } from "./helpers";

/// V74-L3c — the recipe HOME on `kbc-recipe/1`: intent groups, the
/// auto-form + live CLI line, the four result views, and per-step census.
/// The old `recipes/1` panel (`recipes.spec.ts`) is a SEPARATE, unmodified
/// surface on the same page — this spec is scoped to the new runner only.

/// A render bug against REAL corpus data (as opposed to this suite's own
/// smaller assertions) surfaces as `components/ErrorBoundary.tsx`'s
/// fallback (`[data-kbc-error-boundary]`), NOT a silently blank page — the
/// results section is wrapped in its own boundary precisely so a step-
/// rendering bug doesn't blank the header/form above it. Call this right
/// after a card click so a real crash fails with the THROWN message
/// instead of an opaque "element not found" timeout on whatever locator
/// happens to be checked next.
async function failOnErrorBoundary(page: Page) {
  const boundary = page.locator("[data-kbc-error-boundary]");
  if ((await boundary.count()) > 0) {
    const text = await boundary.textContent();
    throw new Error(`recipe page crashed into its ErrorBoundary: ${text}`);
  }
}

test.describe("recipe home (kbc-recipe/1)", () => {
  test("home renders intent groups from the catalog", async ({ page }) => {
    await page.goto(`${BASE}/r/${REPO_NAME}/~recipes`);
    await expect(page.locator("[data-kbc-recipe-home-section]")).toBeVisible({ timeout: 10_000 });
    await expect(page.locator("[data-kbc-recipe-home]")).toBeVisible();
    // The closed five-group taxonomy — at least one group renders with
    // at least one card, without asserting every group is non-empty (a
    // repo's catalog may not populate every intent).
    const cards = page.locator("[data-kbc-recipe-home-card]");
    await expect(cards.first()).toBeVisible({ timeout: 10_000 });
    expect(await cards.count()).toBeGreaterThan(0);
    // The legacy section stays a plain, always-visible page section (this
    // unit's coexistence decision) — never hidden behind the new home.
    await expect(page.locator("[data-kbc-recipes-legacy]")).toBeVisible();
  });

  test("hygiene:aged-todos: pick, run, and a row (if any) opens the reader", async ({ page }) => {
    await page.goto(`${BASE}/r/${REPO_NAME}/~recipes`);
    const card = page.locator('[data-kbc-recipe-home-card="hygiene:aged-todos"]');
    await expect(card).toBeVisible({ timeout: 10_000 });
    await card.click();

    await expect(page.locator('[data-kbc-recipe-run-title="hygiene:aged-todos"]')).toBeVisible({
      timeout: 10_000,
    });
    await failOnErrorBoundary(page);
    // The CLI line composes live from the form's current (default) state —
    // present and naming the slug + repo before any Run click.
    await expect(page.locator("[data-kbc-recipe-cli-bar] code")).toContainText(
      "kb-code recipe run hygiene:aged-todos --repo",
    );

    // Auto-runs on a slug deep-link (no params required for this recipe).
    await expect(
      page.locator("[data-kbc-recipe-steps], [data-kbc-recipes-error], [data-kbc-error-boundary]"),
    ).toBeVisible({ timeout: 15_000 });
    await failOnErrorBoundary(page);

    const rows = page.locator("[data-kbc-recipe-addr-link]");
    const rowCount = await rows.count();
    if (rowCount === 0) {
      // An honestly empty fixture run: the census panel must still name
      // why, never a blank table (this unit's core honesty contract).
      await expect(page.locator("[data-kbc-recipe-census]").first()).toBeVisible();
      await expect(page.locator("[data-kbc-recipe-census-reason-text]").first()).not.toBeEmpty();
      return;
    }

    const firstHref = await rows.first().getAttribute("href");
    expect(firstHref).toBeTruthy();
    await rows.first().click();
    // `firstHref` is the raw `href` attribute (path + query, e.g.
    // `/r/fixture/src/main.rs?line=3`) — `url.pathname` alone never
    // includes the query string, so match both.
    await page.waitForURL((url) => url.pathname + url.search === firstHref, {
      timeout: 10_000,
    });
  });

  test("rails:orphans on a non-Rails fixture: census names the reason honestly", async ({ page }) => {
    await page.goto(`${BASE}/r/${REPO_NAME}/~recipes`);
    const card = page.locator('[data-kbc-recipe-home-card="rails:orphans"]');
    // Not every catalog is guaranteed to ship this slug (a future catalog
    // change is not this test's business) — skip gracefully rather than
    // failing on an unrelated drift.
    if ((await card.count()) === 0) {
      test.skip(true, "rails:orphans not in this repo's catalog");
      return;
    }
    await card.click();
    await expect(page.locator('[data-kbc-recipe-run-title="rails:orphans"]')).toBeVisible({
      timeout: 10_000,
    });
    await failOnErrorBoundary(page);
    await expect(
      page.locator("[data-kbc-recipe-steps], [data-kbc-recipes-error], [data-kbc-error-boundary]"),
    ).toBeVisible({ timeout: 15_000 });
    await failOnErrorBoundary(page);

    const census = page.locator("[data-kbc-recipe-census]").first();
    // The fixture repo is not a Rails app — every step is deterministically
    // empty with the `not-a-rails-app` reason (or the recipe's steps read
    // nothing at all, `no-inputs`); assert the panel is honest either way,
    // never a silent blank table.
    await expect(census).toBeVisible({ timeout: 10_000 });
    await expect(page.locator("[data-kbc-recipe-census-reason-text]").first()).not.toBeEmpty();
  });

  test("copy-cli composes the exact run line for the current form state", async ({ page }) => {
    await page.goto(`${BASE}/r/${REPO_NAME}/~recipes`);
    const card = page.locator('[data-kbc-recipe-home-card="hygiene:aged-todos"]');
    await expect(card).toBeVisible({ timeout: 10_000 });
    await card.click();
    await failOnErrorBoundary(page);
    await expect(page.locator("[data-kbc-recipe-cli-bar]")).toBeVisible({ timeout: 10_000 });
    const line = await page.locator("[data-kbc-recipe-cli-bar] code").textContent();
    expect(line).toMatch(/^kb-code recipe run hygiene:aged-todos --repo /);
  });
});
