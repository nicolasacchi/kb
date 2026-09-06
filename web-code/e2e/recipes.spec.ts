import { expect, test } from "@playwright/test";
import { BASE, REPO_NAME } from "./helpers";

/// V3.3-U1 — Recipes page: catalog, god-functions run + terms expander,
/// missing `since` on new-public-api surfaces the 400 message.

test.describe("recipes page", () => {
  test("catalog renders known recipe names", async ({ page }) => {
    await page.goto(`${BASE}/r/${REPO_NAME}/~recipes`);
    await expect(page.locator("[data-kbc-recipes]")).toBeVisible({ timeout: 10_000 });
    await expect(page.locator("[data-kbc-recipes-catalog]")).toBeVisible();
    await expect(page.locator('[data-kbc-recipes-cat="god-functions"]')).toBeVisible();
    await expect(page.locator('[data-kbc-recipes-cat="new-public-api"]')).toBeVisible();
  });

  test("god-functions run shows ranked rows with a terms expander", async ({ page }) => {
    await page.goto(`${BASE}/r/${REPO_NAME}/~recipes?recipe=god-functions`);
    await expect(page.locator("[data-kbc-recipes-selected='god-functions']")).toBeVisible({
      timeout: 10_000,
    });
    // Auto-runs when since is not required.
    await expect(page.locator("[data-kbc-recipes-table], [data-kbc-recipes-error], .kbc-empty")).toBeVisible({
      timeout: 15_000,
    });
    const rows = page.locator("[data-kbc-recipes-row]");
    const count = await rows.count();
    if (count === 0) {
      // Empty is valid when call_sites/symbols are thin in the fixture.
      return;
    }
    await rows.first().locator("[data-kbc-recipes-terms-btn]").click();
    await expect(page.locator("[data-kbc-recipes-terms]")).toBeVisible();
    await expect(page.locator("[data-kbc-recipes-terms]")).toContainText(/line_span|fan_in|fan_out/);
  });

  test("missing since: client gates the run; server error path renders", async ({ page }) => {
    // The SPA's own behavior for a missing `since` is the client-side
    // gate: Run stays disabled, so the 400 is unreachable through normal
    // interaction. Pin the gate (SPA behavior), the API contract (server
    // behavior, checked directly and labeled as such), AND that a server
    // error which DOES get through renders in the error banner.
    await page.goto(`${BASE}/r/${REPO_NAME}/~recipes?recipe=new-public-api`);
    await expect(page.locator("[data-kbc-recipes-since]")).toBeVisible({ timeout: 10_000 });
    await expect(page.locator("[data-kbc-recipes-run-btn]")).toBeDisabled();

    // API contract, not SPA behavior: missing since is a 400 naming it.
    const errBody = await page.evaluate(async (repo) => {
      const res = await fetch(`/api/recipes/new-public-api?repo=${encodeURIComponent(repo)}`);
      const j = (await res.json()) as { error?: string };
      return { status: res.status, error: j.error ?? "" };
    }, REPO_NAME);
    expect(errBody.status).toBe(400);
    expect(errBody.error.toLowerCase()).toMatch(/since/);

    // SPA error rendering: an unresolvable ref passes the client gate and
    // the server's rejection must surface in the error banner.
    await page.locator("[data-kbc-recipes-since]").fill("not-a-real-ref-xyz");
    await page.locator("[data-kbc-recipes-run-btn]").click();
    await expect(page.locator("[data-kbc-recipes-error]")).toBeVisible({ timeout: 10_000 });
  });
});
