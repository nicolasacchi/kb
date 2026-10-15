import { test, expect } from "@playwright/test";
import { BASE, PORT } from "./helpers";

test.describe("Cmd+K search", () => {
  function port(): number {
    return PORT;
  }

  // Open the cmdk modal. We click the topbar button instead of pressing
  // Ctrl+K because Playwright's keyboard.press needs an explicitly
  // focused element and the synthetic Control+K doesn't reliably reach
  // window-level keydown handlers in headless chromium. The button click
  // path exercises the same setCmdkOpen(true) entry, plus it tests the
  // affordance the user actually sees in the topbar.
  async function openCmdk(page: import("@playwright/test").Page) {
    await page.getByRole("button", { name: /open search/i }).click();
    await expect(page.getByRole("dialog", { name: "search" })).toBeVisible();
  }

  test("opens, Tab cycles modes, results render, Enter navigates", async ({
    page,
  }) => {
    await page.goto(`http://127.0.0.1:${port()}/`);
    await openCmdk(page);
    const dialog = page.getByRole("dialog", { name: "search" });

    // Default mode is hybrid. Tab → keyword (avoids the embedder 400).
    await page.keyboard.press("Tab");
    await expect(dialog.getByRole("button", { name: /mode: keyword/i })).toBeVisible();

    await page.getByRole("textbox", { name: "search query" }).fill("borrow");
    // 150ms debounce + a little request slack.
    await expect(
      dialog.getByRole("option", { name: /Borrow Checker/ }),
    ).toBeVisible({ timeout: 5_000 });

    await page.keyboard.press("Enter");
    // Track U — Enter navigates to the path-based permalink (ends .html),
    // not the legacy 12-hex id.
    await expect(page).toHaveURL(/\/a\/canon\/[^/]+\.html(\?|$)/);
  });

  test("Esc closes the modal", async ({ page }) => {
    await page.goto(`http://127.0.0.1:${port()}/`);
    await openCmdk(page);
    const dialog = page.getByRole("dialog", { name: "search" });
    await page.keyboard.press("Escape");
    await expect(dialog).not.toBeVisible();
  });

  test("hybrid mode without an embedder surfaces the 400 inline", async ({
    page,
  }) => {
    await page.goto(`http://127.0.0.1:${port()}/`);
    await openCmdk(page);
    await page.getByRole("textbox", { name: "search query" }).fill("borrow");
    // Stays in hybrid (default). e2e fixture has no embedding_model.
    const dialog = page.getByRole("dialog", { name: "search" });
    await expect(
      dialog.getByRole("alert").getByText(/embedding_model/),
    ).toBeVisible({ timeout: 5_000 });
  });
});

// OL S3 — kb's chords teach themselves: the shared which-key overlay and help sheet.
test.describe("which-key overlay and help sheet", () => {
  test("g pending shows the overlay after ~400ms; a navigates to the atlas", async ({ page }) => {
    await page.goto(`${BASE}/`);
    await page.locator("body").click({ position: { x: 5, y: 5 } });
    await page.keyboard.press("g");
    const overlay = page.locator("[data-kbc-whichkey]");
    await expect(overlay).toBeVisible();
    await expect(overlay).toContainText(/atlas/i);
    await page.keyboard.press("a");
    await expect(page).toHaveURL(/view=atlas/);
    await expect(overlay).toHaveCount(0);
  });

  test("Escape clears the overlay with the chord", async ({ page }) => {
    await page.goto(`${BASE}/`);
    await page.locator("body").click({ position: { x: 5, y: 5 } });
    await page.keyboard.press("g");
    await expect(page.locator("[data-kbc-whichkey]")).toBeVisible();
    await page.keyboard.press("Escape");
    await expect(page.locator("[data-kbc-whichkey]")).toHaveCount(0);
  });

  test("? opens the shared help sheet with gallery and reader sections", async ({ page }) => {
    await page.goto(`${BASE}/`);
    await page.locator("body").click({ position: { x: 5, y: 5 } });
    await page.keyboard.press("?");
    const sheet = page.getByRole("dialog", { name: "keyboard shortcuts" });
    await expect(sheet).toBeVisible();
    await expect(sheet.locator(".kbc-kbdhelp__group h3", { hasText: /^Gallery$/ })).toBeVisible();
    await expect(sheet.locator(".kbc-kbdhelp__group h3", { hasText: /^Reader$/ })).toBeVisible();
    await page.keyboard.press("Escape");
    await expect(sheet).toHaveCount(0);
  });
});
