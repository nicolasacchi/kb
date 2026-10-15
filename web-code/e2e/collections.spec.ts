import { expect, test } from "@playwright/test";
import { BASE, REPO_NAME } from "./helpers";

/// OL-C1 — the page behind the Collections door.
test.describe("collections page", () => {
  test.use({ viewport: { width: 1280, height: 720 } });

  test("three labelled groups render with a card per destination", async ({ page }) => {
    await page.goto(`${BASE}/r/${REPO_NAME}/~collections`);
    await expect(page.getByRole("heading", { level: 1 })).toContainText("Collections");
    for (const [group, label, cards] of [
      ["sets", "Sets", 4],
      ["boards", "Boards", 2],
      ["instruments", "Instruments", 6],
    ] as const) {
      const section = page.locator(`[data-kbc-collections-group="${group}"]`);
      await expect(section.getByRole("heading", { level: 2 })).toHaveText(label);
      await expect(section.locator("[data-kbc-collection]")).toHaveCount(cards);
    }
    // The door is lit while on its own page.
    await expect(page.locator("[data-kbc-topbar-collections]")).toHaveAttribute(
      "aria-current",
      "page",
    );
  });

  test("a card navigates to its destination", async ({ page }) => {
    await page.goto(`${BASE}/r/${REPO_NAME}/~collections`);
    await page.locator('[data-kbc-collection="todos"]').click();
    await expect(page).toHaveURL(new RegExp(`/r/${REPO_NAME}/~todos`));
  });

  test("the Collections door lands on the page", async ({ page }) => {
    await page.goto(`${BASE}/r/${REPO_NAME}`);
    await page.locator("[data-kbc-topbar-collections]").click();
    await expect(page).toHaveURL(new RegExp(`/r/${REPO_NAME}/~collections`));
    await expect(page.locator("[data-kbc-collections]")).toBeVisible();
  });
});
