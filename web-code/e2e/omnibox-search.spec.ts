import { expect, test } from "@playwright/test";
import { KNOWN_FILE, KNOWN_SYMBOL, TEXT_NEEDLE } from "./fixture-repo";
import { BASE, REPO_NAME } from "./helpers";

test.describe("omnibox search", () => {
  test("Ctrl+K opens the omnibox, a symbol query finds it, Enter opens the reader at the right file", async ({
    page,
  }) => {
    await page.goto(`${BASE}/`);
    await page.keyboard.press("Control+k");

    const dialog = page.getByRole("dialog", { name: "search" });
    await expect(dialog).toBeVisible();

    const input = dialog.getByRole("textbox", { name: "search query" });
    await input.fill(`@${KNOWN_SYMBOL}`);

    const symbolsSection = dialog.locator('[data-kbc-lane="symbols"]');
    await expect(symbolsSection).toContainText(KNOWN_SYMBOL, { timeout: 10_000 });

    // The prefix-scoped query narrows to a single lane section — its
    // header is the initial cursor, one ArrowDown reaches the first
    // (only) hit row.
    await page.keyboard.press("ArrowDown");
    await page.keyboard.press("Enter");

    await expect(page).toHaveURL(new RegExp(`/r/${REPO_NAME}/${KNOWN_FILE}`));
    await expect(dialog).toBeHidden();
    await expect(page.locator(".kbc-codeview")).toContainText(KNOWN_SYMBOL);
  });

  test("a plain (unprefixed) query renders every fixed lane section, and Esc closes the omnibox", async ({
    page,
  }) => {
    await page.goto(`${BASE}/`);
    await page.keyboard.press("Control+k");
    const dialog = page.getByRole("dialog", { name: "search" });
    const input = dialog.getByRole("textbox", { name: "search query" });
    await input.fill(TEXT_NEEDLE);

    // The server always returns the full six-lane section set for an
    // unprefixed query, even when some lanes report `unavailable_reason`
    // (semantic is disabled by default; sessions gets a `BadStatus` —
    // W2.B.R fix 14: since DCB W2.B, `[kb_daemon]` IS enabled and reachable
    // in this harness, pointed at `doclens-fixture.ts`'s mock, but that
    // mock implements only the doc-lens `code-refs`/`by-path` routes, not
    // `/api/sessions/recollect` — the mock's plain 404 for that path is
    // what degrades the sessions lane here, not an unreachable daemon) —
    // see `search::unified::run`'s doc.
    await expect(dialog.locator("[data-kbc-lane]")).toHaveCount(6, { timeout: 10_000 });
    await expect(dialog.locator('[data-kbc-lane="text"]')).toContainText(TEXT_NEEDLE);

    await page.keyboard.press("Escape");
    await expect(dialog).toBeHidden();
  });
});
