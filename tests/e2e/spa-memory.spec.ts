import { test, expect } from "@playwright/test";
import { PORT } from "./helpers";

// M7 — the /memory view renders the seeded global-scope memory corpus
// (2 artifacts from global-setup), shows the scope control, and forgets
// a memory live (the memory.forgotten SSE refetches the list — no reload).
// MI-W3.R adds coverage for the inline SalienceEdit control (click-to-edit
// number input → PATCH …/salience) — placed FIRST in the describe so it
// runs against the untouched corpus, before the forget test below removes
// a row.
test.describe("memory view", () => {
  test("edits a memory's salience via the inline control and the row reflects the new value", async ({
    page,
  }) => {
    await page.goto(`http://127.0.0.1:${PORT}/memory`);

    // "Deploy pipeline" is seeded with salience 0.6 (see global-setup.ts).
    const row = page.getByTestId("memory-item").filter({ hasText: "Deploy pipeline" });
    await expect(row).toBeVisible({ timeout: 10_000 });

    const valueBtn = row.getByTestId("memory-salience-value");
    await expect(valueBtn).toHaveText("0.60");

    await valueBtn.click();
    const input = row.getByTestId("memory-salience-input");
    await input.fill("0.80");
    await input.press("Enter");

    await expect(valueBtn).toHaveText("0.80", { timeout: 10_000 });

    // Persisted server-side (not just an optimistic client artifact) —
    // survives a reload that re-fetches recall from scratch.
    await page.reload();
    const rowAfterReload = page
      .getByTestId("memory-item")
      .filter({ hasText: "Deploy pipeline" });
    await expect(rowAfterReload.getByTestId("memory-salience-value")).toHaveText("0.80", {
      timeout: 10_000,
    });
  });

  test("renders memories, scope control, and forgets live", async ({ page }) => {
    await page.goto(`http://127.0.0.1:${PORT}/memory`);

    await expect(page.getByTestId("memory-view")).toBeVisible();
    await expect(page.getByTestId("memory-scope-global")).toBeVisible();

    const items = page.getByTestId("memory-item");
    await expect(items.first()).toBeVisible({ timeout: 10_000 });
    const before = await items.count();
    expect(before).toBeGreaterThanOrEqual(2);

    // Forget the first memory; the daemon emits memory.forgotten, the
    // useMemories hook refetches recall, and the row drops without a
    // manual reload.
    await items.first().getByTestId("memory-forget").click();
    await expect(items).toHaveCount(before - 1, { timeout: 10_000 });
  });
});
