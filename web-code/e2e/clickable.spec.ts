import { expect, test } from "@playwright/test";
import { CALLER_FILE, KNOWN_FILE, KNOWN_SYMBOL } from "./fixture-repo";
import { BASE, REPO_NAME } from "./helpers";

/// B1's tier-0 clickable code: the `gd`/`gr`/`K` peek panel over the
/// EXISTING `/api/defs` + `/api/xrefs` endpoints, and the linkify layer for
/// comment-embedded paths/URLs. `caller.rs` (added additively to
/// `fixture-repo.ts`) calls `KNOWN_SYMBOL` twice and carries a comment
/// naming `KNOWN_FILE` plus a URL — `lib.rs`'s own line numbers (several
/// OTHER specs pin those) are untouched.

async function openCallerFile(page: import("@playwright/test").Page) {
  await page.goto(`${BASE}/r/${REPO_NAME}`);
  await page.locator(".kbc-tree__row", { hasText: CALLER_FILE }).click();
  await expect(page.locator(".kbc-codeview")).toContainText(KNOWN_SYMBOL, { timeout: 10_000 });
}

/// Click directly on a `KNOWN_SYMBOL` call-site identifier — tree-sitter's
/// bundled `highlights.scm` tags a `call_expression`'s callee as its own
/// `@function` node (`queries/highlights.scm`'s `(call_expression function:
/// (identifier) @function)`), so it renders as its own element with EXACTLY
/// this text; an exact `getByText` match places the click (and therefore
/// the cursor) squarely on the word, no pixel-math guessing required.
async function clickKnownSymbolUsage(page: import("@playwright/test").Page) {
  await page.locator(".kbc-codeview").getByText(KNOWN_SYMBOL, { exact: true }).first().click();
}

test.describe("clickable code (B1)", () => {
  test("gd on a single exact definition opens it in place — no peek panel, no navigation", async ({ page }) => {
    await openCallerFile(page);
    const before = page.url();
    await clickKnownSymbolUsage(page);

    await page.keyboard.press("g");
    await page.keyboard.press("d");

    // `KNOWN_SYMBOL` is defined exactly once (`lib.rs`, line 1) — a single
    // exact, same-repo match no longer navigates away (V70-A6 §P7): it
    // opens `lib.rs`'s own source as an INLINE peek under the caret line,
    // so the call site in `caller.rs` is never lost (`peek-inline.spec.ts`
    // pins the mechanism itself; this only pins that B1's tier-0 path feeds
    // it the same way `resolve.spec.ts`'s position-based path does).
    const peek = page.locator("[data-kbc-inpeek]");
    await expect(peek).toBeVisible({ timeout: 10_000 });
    await expect(peek.locator("[data-kbc-inpeek-body]")).toContainText(KNOWN_SYMBOL, { timeout: 10_000 });
    expect(new URL(page.url()).pathname).toBe(new URL(before).pathname);
    await expect(page.locator("[data-kbc-peek]")).toHaveCount(0);
  });

  test("gr opens the peek panel with >= 2 references and the approximate badge; Esc closes it", async ({
    page,
  }) => {
    await openCallerFile(page);
    await clickKnownSymbolUsage(page);

    await page.keyboard.press("g");
    await page.keyboard.press("r");

    const panel = page.locator("[data-kbc-peek]");
    await expect(panel).toBeVisible();
    await expect(panel).toHaveAttribute("data-kbc-peek-mode", "refs");

    // References are a repo-wide grep (`lib.rs`'s declaration + `helper()`'s
    // call, plus `caller.rs`'s own two calls) — at least 2, well past the
    // single-exact-match shortcut `gd` uses.
    await expect(panel.locator(".kbc-peek__row").first()).toBeVisible({ timeout: 10_000 });
    const rowCount = await panel.locator(".kbc-peek__row").count();
    expect(rowCount).toBeGreaterThanOrEqual(2);

    // Text-grep is ALWAYS approximate (`agentview::xref::REFS_NOTE`) — the
    // honesty badge must be visible, never hidden.
    await expect(panel.locator("[data-kbc-peek-badge]")).toBeVisible();

    await page.keyboard.press("Escape");
    await expect(panel).toHaveCount(0);
    // Esc returns focus to the buffer — the vim keymap should see keys
    // again immediately (a stray leftover panel would swallow this `g`).
    await page.keyboard.press("g");
    await page.keyboard.press("g");
    await expect(page).toHaveURL(/[?&]line=1(&|$)/, { timeout: 4_000 });
  });

  test("clicking the lib.rs path token in the comment navigates to lib.rs", async ({ page }) => {
    await openCallerFile(page);

    const pathToken = page.locator(".kbc-link-token", { hasText: KNOWN_FILE });
    await expect(pathToken).toBeVisible();
    await expect(pathToken).toHaveAttribute("data-kbc-link-kind", "path");
    await pathToken.click();

    await expect(page).toHaveURL(new RegExp(`/r/${REPO_NAME}/${KNOWN_FILE}(\\?|$)`), { timeout: 5_000 });
    await expect(page.locator(".kbc-codeview")).toContainText(KNOWN_SYMBOL, { timeout: 10_000 });
  });

  test("the comment's URL renders as its own linkify token, distinct from the path token", async ({ page }) => {
    await openCallerFile(page);

    const urlToken = page.locator('.kbc-link-token[data-kbc-link-kind="url"]');
    await expect(urlToken).toBeVisible();
    await expect(urlToken).toContainText("https://example.com");
  });
});
