import { expect, test, type APIRequestContext } from "@playwright/test";
import { BASE, REPO_NAME } from "./helpers";
import { KNOWN_FILE } from "./fixture-repo";

/// V74-L2 — add-to-board from the ACTION PANEL, end to end.
///
/// The point of the spec is that the row is not a per-surface button: it is the
/// server-rendered actions/1 row `collect.board`, so the `.` panel, the
/// right-click menu and the drag-select pill all offer it from ONE list. What
/// is asserted here is the whole round trip — the row is present, choosing it
/// opens the picker, applying composes a WHOLE kbc-canvas/1 document (there is
/// no partial-patch route), and the new card is on the board afterwards.
///
/// `collect.board` is a MUTATING row, so it is absent for a caller the daemon
/// has not cleared. This harness hits 127.0.0.1, so it is present — which is
/// itself worth asserting, because the same row must NOT be offered where the
/// apply would 404.

const SLUG = "e2e-add-board";

async function seedBoard(request: APIRequestContext) {
  const res = await request.post(`${BASE}/api/boards/apply`, {
    headers: { "X-Kbc-Request": "1", "Content-Type": "application/json" },
    data: {
      schema: "kbc-canvas/1",
      repo: REPO_NAME,
      slug: SLUG,
      title: "Add target",
      nodes: [{ id: "seed", kind: "note", title: "seed", body_md: "a board never starts empty" }],
    },
  });
  expect(res.status(), await res.text()).toBeLessThan(300);
}

test.describe("add to board", () => {
  test.beforeAll(async ({ request }) => {
    await seedBoard(request);
  });

  test.afterAll(async ({ request }) => {
    await request
      .delete(`${BASE}/api/boards/${SLUG}?repo=${REPO_NAME}`, {
        headers: { "X-Kbc-Request": "1" },
      })
      .catch(() => undefined);
  });

  test("the `.` panel offers `collect.board`, and applying it lands a card", async ({ page }) => {
    await page.goto(`${BASE}/r/${REPO_NAME}/${KNOWN_FILE}`);
    await expect(page.locator(".kbc-codeview").first()).toBeVisible({ timeout: 20_000 });

    // Put the caret somewhere real, then open the ONE action panel.
    await page.locator(".kbc-codeview .cm-content").first().click();
    await page.keyboard.press(".");
    const menu = page.locator("[data-kbc-action-menu]");
    await expect(menu).toBeVisible({ timeout: 15_000 });

    // The row came from the server's list, not from this page.
    const row = menu.locator('[data-kbc-action="collect.board"]');
    await expect(row).toBeVisible();
    await expect(row).toContainText("Add to board");
    await row.click();

    // The picker: choose the seeded board rather than making a new one.
    const dialog = page.locator("[data-kbc-addboard]");
    await expect(dialog).toBeVisible({ timeout: 10_000 });
    await expect(dialog.locator("[data-kbc-addboard-target]")).toContainText(KNOWN_FILE);
    await dialog.locator("[data-kbc-addboard-select]").selectOption(SLUG);
    await dialog.locator("[data-kbc-addboard-submit]").click();

    // Applying navigates to the board, and the new card is there beside the
    // seed — the composition sent the WHOLE document, so nothing was lost.
    await expect(page).toHaveURL(new RegExp(`~boards/${SLUG}`), { timeout: 20_000 });
    await expect(page.locator('[data-kbc-board-node="seed"]')).toBeVisible({ timeout: 15_000 });
    const added = page.locator('[data-kbc-board-node][data-kbc-board-kind="code"]').first();
    await expect(added).toBeVisible();
    await expect(added).toContainText(KNOWN_FILE.replace(/\.rs$/, ""));
    // It is a REFERENCE the daemon re-resolved, not a copy: it carries a state
    // and a reason like every other card.
    await expect(added).toHaveAttribute("data-kbc-board-state", /pinned|carried|orphan/);

    // The board's own census counts it.
    await expect(page.locator("[data-kbc-board-census]")).toContainText("2 nodes");
  });

  test("`Space b a` opens the same picker without the menu", async ({ page }) => {
    await page.goto(`${BASE}/r/${REPO_NAME}/${KNOWN_FILE}`);
    await expect(page.locator(".kbc-codeview").first()).toBeVisible({ timeout: 20_000 });
    await page.locator(".kbc-codeview .cm-content").first().click();

    // The leader asks the same `/api/actions` the panel asks and takes the
    // DEFAULT target, so the two can never disagree about what "this" is.
    await page.keyboard.press(" ");
    await page.keyboard.press("b");
    await page.keyboard.press("a");
    const dialog = page.locator("[data-kbc-addboard]");
    await expect(dialog).toBeVisible({ timeout: 15_000 });
    await expect(dialog.locator("[data-kbc-addboard-target]")).toContainText(KNOWN_FILE);
    // Cancel: nothing is written until the picker is submitted.
    await dialog.locator("[data-kbc-addboard-close]").click();
    await expect(dialog).toHaveCount(0);
  });
});
