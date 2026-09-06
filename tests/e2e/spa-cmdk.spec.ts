import { test, expect } from "@playwright/test";
import { PORT } from "./helpers";

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
