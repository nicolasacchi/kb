import { expect, test } from "@playwright/test";
import { BASE } from "./helpers";

/// OL-C8 — the settings page: five plain sections, the shared theme picker
/// inline, and the review-store diagnostics Home used to carry.
test.describe("Settings page", () => {
  test("renders all five section headings", async ({ page }) => {
    await page.goto(`${BASE}/~settings`);
    await expect(page.getByRole("heading", { level: 1, name: "Settings" })).toBeVisible();
    for (const name of ["Appearance", "Keyboard", "Repositories", "Review store", "Sibling"]) {
      await expect(page.getByRole("heading", { level: 2, name })).toBeVisible();
    }
  });

  test("choosing a theme family updates <html data-kbc-theme>", async ({ page }) => {
    await page.goto(`${BASE}/~settings`);
    const rows = page.locator("[data-kbc-settings-section=appearance] [data-kbc-theme-family]");
    await expect(rows.first()).toBeVisible();
    const html = page.locator("html");
    const before = await html.getAttribute("data-kbc-theme");
    const count = await rows.count();
    let changed = false;
    for (let i = 0; i < count && !changed; i++) {
      await rows.nth(i).click();
      const now = await html.getAttribute("data-kbc-theme");
      changed = now !== before;
    }
    expect(changed).toBe(true);
  });

  test("the review-store section shows the remote-mutations line and the per-repo store", async ({
    page,
  }) => {
    await page.goto(`${BASE}/~settings`);
    const section = page.locator("[data-kbc-settings-section=review-store]");
    await expect(section.locator("[data-kbc-home-remote-mutations]")).toContainText(
      "Remote review mutations:",
    );
    await expect(section.locator("[data-kbc-home-store]").first()).toBeVisible({ timeout: 10_000 });
  });

  test("the top bar gear opens Settings", async ({ page }) => {
    await page.goto(`${BASE}/`);
    await page.locator("[data-kbc-topbar-settings]").click();
    await expect(page).toHaveURL(/\/~settings$/);
  });
});
