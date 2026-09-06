import { expect, test } from "@playwright/test";
import { TODOS_FILE } from "./fixture-repo";
import { BASE, REPO_NAME } from "./helpers";

/// V3.N2 — TODOs page: groups, marker chip filters, row navigates to file:line.

test.describe("todos page", () => {
  test("renders groups, filters by marker chip, row navigates", async ({ page }) => {
    await page.goto(`${BASE}/r/${REPO_NAME}/~todos`);
    await expect(page.locator("[data-kbc-todos]")).toBeVisible({ timeout: 10_000 });

    // Groups should include the dedicated TODOS_FILE once the index has
    // extracted its comment markers (written by fixture-repo.ts).
    const group = page.locator(`[data-kbc-todos-group="${TODOS_FILE}"]`);
    await expect(group).toBeVisible({ timeout: 30_000 });

    // Marker chips derived from data include TODO.
    const todoChip = page.locator('[data-kbc-todos-chip="TODO"]');
    await expect(todoChip).toBeVisible();
    await todoChip.click();
    await expect(todoChip).toHaveClass(/is-on/);

    // Only TODO rows remain under the group.
    const rows = page.locator("[data-kbc-todos-row]");
    await expect(rows.first()).toBeVisible();
    await expect(rows.first().locator("[data-kbc-todos-marker]")).toHaveAttribute(
      "data-kbc-todos-marker",
      "TODO",
    );

    // Row click → reader at that line.
    await rows.first().click();
    await expect(page).toHaveURL(new RegExp(`/r/${REPO_NAME}/${TODOS_FILE}`));
    await expect(page).toHaveURL(/[?&]line=\d+/);
    await expect(page.locator(".kbc-codeview")).toBeVisible({ timeout: 10_000 });
  });
});
