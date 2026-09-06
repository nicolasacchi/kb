import { expect, test } from "@playwright/test";
import { TEXT_NEEDLE } from "./fixture-repo";
import { BASE } from "./helpers";

test.describe("omnibox keyboard model", () => {
  test("Tab moves the active section forward, ArrowDown moves into its rows, Shift+Tab moves back", async ({
    page,
  }) => {
    await page.goto(`${BASE}/`);
    await page.keyboard.press("Control+k");
    const dialog = page.getByRole("dialog", { name: "search" });
    const input = dialog.getByRole("textbox", { name: "search query" });
    await input.fill(TEXT_NEEDLE);

    // Six lanes render for an unprefixed query — wait for the fixed set
    // before driving the keyboard so the reducer's SET_SECTIONS has
    // already run.
    await expect(dialog.locator("[data-kbc-lane]")).toHaveCount(6, { timeout: 10_000 });

    const filesHeader = dialog.locator('[data-kbc-lane="files"] [data-kbc-role="header"]');
    const symbolsHeader = dialog.locator('[data-kbc-lane="symbols"] [data-kbc-role="header"]');
    const textHeader = dialog.locator('[data-kbc-lane="text"] [data-kbc-role="header"]');

    // Cursor starts on the first lane's (files) header.
    await expect(filesHeader).toHaveClass(/is-active/);

    await page.keyboard.press("Tab");
    await expect(symbolsHeader).toHaveClass(/is-active/);
    await expect(filesHeader).not.toHaveClass(/is-active/);

    await page.keyboard.press("Tab");
    await expect(textHeader).toHaveClass(/is-active/);

    // ArrowDown moves into the text lane's own rows (it has a match for
    // TEXT_NEEDLE), not across to another section.
    await page.keyboard.press("ArrowDown");
    const firstTextRow = dialog.locator('[data-kbc-lane="text"] [data-kbc-role="row"][data-kbc-row="0"]');
    await expect(firstTextRow).toHaveClass(/is-active/);
    await expect(textHeader).not.toHaveClass(/is-active/);

    await page.keyboard.press("Shift+Tab");
    await expect(symbolsHeader).toHaveClass(/is-active/);
  });
});
