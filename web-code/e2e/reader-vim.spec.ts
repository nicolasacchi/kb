import { expect, test } from "@playwright/test";
import { KNOWN_FILE, KNOWN_SYMBOL } from "./fixture-repo";
import { BASE, REPO_NAME } from "./helpers";

/// A3's vim reading buffer, end-to-end: the buffer is the primary focus
/// surface (focus lands in it when a file opens), the modal keymap moves a
/// REAL cursor, position round-trips through the `?line=` URL param (the
/// address bar is always a permalink), `Y` copies it, `?` opens the
/// cheatsheet, and a hard navigation to a line-RANGE deep link — including
/// a dotted source-file path, which `spa.rs::is_client_route` carves out of
/// the asset split — selects the range on load.

/// The cursor→URL debounce is 500ms (`lib/cursorUrlSync.ts`); every URL
/// assertion polls well past it.
const URL_SYNC_TIMEOUT = 4_000;

async function openFixtureFile(page: import("@playwright/test").Page) {
  await page.goto(`${BASE}/r/${REPO_NAME}`);
  await page.locator(".kbc-tree__row", { hasText: KNOWN_FILE }).click();
  await expect(page.locator(".kbc-codeview")).toContainText(KNOWN_SYMBOL, { timeout: 10_000 });
  // Deterministic focus + cursor position for the keys that follow —
  // clicking line 1's text is equivalent to the focus-follows-file effect
  // but not racy against it.
  await page.locator(".kbc-codeview .cm-line").first().click();
}

test.describe("vim reading buffer", () => {
  test("moves a real cursor, syncs ?line= both ways, copies a permalink", async ({ page }) => {
    await openFixtureFile(page);

    // G — jump to the last line; the cursor position flows into the URL
    // after the debounce.
    await page.keyboard.press("G");
    await expect(page).toHaveURL(/[?&]line=\d+/, { timeout: URL_SYNC_TIMEOUT });
    const bottomLine = new URL(page.url()).searchParams.get("line");
    expect(Number(bottomLine)).toBeGreaterThan(1);

    // gg — back to the top; the SAME param updates in place (replace, not
    // push — history length must not grow per cursor move).
    await page.keyboard.press("g");
    await page.keyboard.press("g");
    await expect(page).toHaveURL(/[?&]line=1(&|$)/, { timeout: URL_SYNC_TIMEOUT });

    // v + j — visual mode over two lines: the status chip appears, and the
    // URL picks up the RANGE grammar.
    await page.keyboard.press("v");
    const status = page.locator("[data-kbc-vim-status]");
    await expect(status).toHaveText(/VISUAL/);
    await page.keyboard.press("j");
    await expect(page).toHaveURL(/[?&]line=1-2(&|$)/, { timeout: URL_SYNC_TIMEOUT });

    // Y — permalink copied (the transient chip is the observable; clipboard
    // contents aren't readable without extra permissions).
    await page.keyboard.press("Y");
    await expect(page.locator("[data-kbc-copied]")).toBeVisible();

    // Esc — back to normal mode; the chip-less steady state returns.
    await page.keyboard.press("Escape");
    await expect(status).toHaveCount(0);
  });

  test("a hard-navigated line-range deep link selects the range on load", async ({ page }) => {
    // Dotted source-file path + query — served as the SPA shell by
    // `spa.rs`'s `/r/` carve-out, then `Reader`'s ?line= effect drives the
    // selection. This is exactly the URL the `Y` action copies.
    await page.goto(`${BASE}/r/${REPO_NAME}/${KNOWN_FILE}?line=2-4`);
    await expect(page.locator(".kbc-codeview")).toContainText(KNOWN_SYMBOL, { timeout: 10_000 });
    // drawSelection renders the non-collapsed selection as background
    // layers — present only when a real range is selected.
    await expect(page.locator(".kbc-codeview .cm-selectionBackground").first()).toBeVisible({
      timeout: 5_000,
    });
  });

  test("/ opens the in-buffer search panel; ? opens the cheatsheet", async ({ page }) => {
    await openFixtureFile(page);

    await page.keyboard.press("/");
    const panel = page.locator(".kbc-codeview .cm-panel.cm-search");
    await expect(panel).toBeVisible();
    // Escape inside the panel's input closes it (searchKeymap's
    // panel-scoped binding — the vim handler deliberately ignores typing
    // targets).
    await panel.locator("input[name=search]").press("Escape");
    await expect(panel).toHaveCount(0);

    // `?` from the buffer opens the cheatsheet; Escape dismisses it.
    await page.keyboard.press("?");
    const help = page.getByRole("dialog", { name: "keyboard shortcuts" });
    await expect(help).toBeVisible();
    await page.keyboard.press("Escape");
    await expect(help).toHaveCount(0);
  });
});
