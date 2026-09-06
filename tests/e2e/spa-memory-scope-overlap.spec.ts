import { test, expect } from "@playwright/test";
import { PORT } from "./helpers";

// MI-W4.7 — cross-kb scope overlap: an UpSet-style aggregation over the
// memory population's global/linked_kbs scope, computed client-side from
// the SAME hits the /memory table renders. The seeded `mem` kb's two
// memories ("Prefers tabs", "Deploy pipeline") are both global-scope.

test.describe("cross-kb scope overlap — MI-W4.7", () => {
  test("renders one row for the seeded corpus's global memories and pivots the ?kb= lens on click", async ({
    page,
  }) => {
    await page.goto(`http://127.0.0.1:${PORT}/memory`);
    const overlap = page.getByTestId("scope-overlap");
    await expect(overlap).toBeVisible({ timeout: 10_000 });

    const rows = overlap.getByTestId("scope-overlap-row");
    await expect(rows.first()).toBeVisible();
    // Both seeded memories are global-scope — the global row shows.
    await expect(rows.filter({ hasText: "★ global" })).toBeVisible();

    await rows.filter({ hasText: "★ global" }).click();
    await expect(page).toHaveURL(/[?&]kb=mem(&|$)/);
    await expect(page.getByTestId("memory-kb-lens")).toBeVisible();
  });
});
