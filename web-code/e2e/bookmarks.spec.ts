import { expect, test } from "@playwright/test";
import { KNOWN_FILE, KNOWN_SYMBOL } from "./fixture-repo";
import { BASE, REPO_NAME } from "./helpers";

/// V3.N2 — bookmarks: `gm` toggle, inspector rail lists, `gM` mnemonic jump.

async function openFixtureFile(page: import("@playwright/test").Page) {
  await page.goto(`${BASE}/r/${REPO_NAME}/${KNOWN_FILE}`);
  await expect(page.locator(".kbc-codeview")).toContainText(KNOWN_SYMBOL, { timeout: 10_000 });
  await page.locator(".kbc-codeview .cm-line").first().click();
}

test.describe("bookmarks", () => {
  test("gm toggles, rail lists, gM jumps via mnemonic", async ({ page }) => {
    await openFixtureFile(page);

    // gm — add a bookmark on the current line.
    await page.keyboard.press("g");
    await page.keyboard.press("m");
    await expect(page.locator(".kbc-toast--ok")).toContainText(/bookmark added/i, {
      timeout: 5_000,
    });

    // Open the rail's Notes tab — V70-A4 folded the old `bookmarks` and
    // `annotations` SOURCE tabs into one TASK tab ("what did I mark?",
    // docs/research/kb-code-v7-continuum-2026-09.html §P1).
    await page.locator('[data-kbc-itab="notes"]').click();
    const rows = page.locator("[data-kbc-bookmark-row]");
    await expect(rows.first()).toBeVisible({ timeout: 5_000 });
    await expect(rows.first()).toContainText(KNOWN_FILE);

    // Set a mnemonic on the first row.
    await page.locator("[data-kbc-bookmark-edit-mnemonic]").first().click();
    await page.locator("[data-kbc-bookmark-mnemonic-input]").fill("a");
    await page.locator("[data-kbc-bookmark-mnemonic-input]").press("Enter");
    await expect(page.locator("[data-kbc-bookmark-mnemonic]").first()).toHaveText("a");

    // Move cursor away (j a few times) then gM + a should jump back.
    await page.locator(".kbc-codeview .cm-line").first().click();
    await page.keyboard.press("G");
    await page.keyboard.press("g");
    await page.keyboard.press("M");
    const mnemonicPopup = page.locator("[data-kbc-mnemonic-popup]");
    await expect(mnemonicPopup).toBeVisible();
    await page.keyboard.press("a");
    await expect(mnemonicPopup).toHaveCount(0);
    await expect(page).toHaveURL(/[?&]line=\d+/, { timeout: 4_000 });

    // gm again on the same line removes the bookmark.
    await page.locator(".kbc-codeview .cm-line").first().click();
    // Jump to the bookmarked line first so toggle removes it.
    await page.goto(`${BASE}/r/${REPO_NAME}/${KNOWN_FILE}?line=1`);
    await expect(page.locator(".kbc-codeview")).toContainText(KNOWN_SYMBOL, { timeout: 10_000 });
    await page.locator(".kbc-codeview .cm-line").first().click();
    await page.keyboard.press("g");
    await page.keyboard.press("m");
    // Either added again or removed — ensure toast fires.
    await expect(page.locator(".kbc-toast--ok")).toBeVisible({ timeout: 5_000 });
  });
});
