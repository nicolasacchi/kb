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

  // V71-E2 (`cdba1573`) repointed `gr` — this test's rewrite is spec DRIFT,
  // not a regression. B1's `gr` opened the peek popup over `/api/xrefs`, a
  // word-boundary grep whose whole result set was flagged `approximate`.
  // E2 repointed all three entries (`gr`, the gutter lens chip, the peek
  // card's `u`) onto `/api/usages/2` — V71-E1's classified ladder — and
  // landed them in the bottom DRAWER, where `desk/placement.ts` has said
  // usages belong since V70-A4. There is no peek panel on this path any
  // more. What the test pins is unchanged in substance: ≥2 references, and
  // the honesty surface that says how far to trust them, on screen rather
  // than implied. The grep lane is not deleted, it is DEMOTED to an
  // explicit "mentions" chip fetched only when switched on and counted in
  // its own field — E2's answer to the three disagreeing "usages" numbers
  // recon §6.3 recorded.
  test("gr lands >= 2 classified usages in the drawer dock, with the trust census on screen", async ({
    page,
  }) => {
    await openCallerFile(page);
    await clickKnownSymbolUsage(page);

    await page.keyboard.press("g");
    await page.keyboard.press("r");

    // No popup: the dock IS the surface, and the drawer opens to hold it.
    const dock = page.locator("[data-kbc-usages-dock]");
    await expect(dock).toBeVisible({ timeout: 10_000 });
    await expect(page.locator("[data-kbc-peek]")).toHaveCount(0);
    await expect(page.locator("[data-region='drawer']")).toHaveAttribute(
      "data-desk-drawer-collapsed",
      "0",
    );

    // The same references B1 counted (`lib.rs`'s declaration + `helper()`'s
    // call, plus `caller.rs`'s own two calls, plus `impact_extra.rs`'s) — at
    // least 2, well past the single-exact-match shortcut `gd` uses.
    await expect(dock.locator("[data-kbc-usages-row]").first()).toBeVisible({ timeout: 10_000 });
    const rowCount = await dock.locator("[data-kbc-usages-row]").count();
    expect(rowCount).toBeGreaterThanOrEqual(2);

    // The honesty surface, in the vocabulary that replaced the whole-set
    // `approximate` badge: a census reporting the SERVER's own totals (never
    // a count derived from the page it happens to hold), and a per-row trust
    // class — every row here landing at `likely`, because the static ladder
    // cannot reach `exact` for Rust without a SCIP index and says so rather
    // than rounding up.
    await expect(dock.locator("[data-kbc-usages-total]")).toContainText(`${rowCount} usages`);
    await expect(dock.locator("[data-kbc-usages-shown]")).toContainText(
      `showing ${rowCount} of ${rowCount} fetched`,
    );
    await expect(dock.locator("[data-kbc-usages-row] [data-kbc-trust]")).toHaveCount(rowCount);
    await expect(dock.locator('[data-kbc-usages-row] [data-kbc-trust="likely"]')).toHaveCount(
      rowCount,
    );
    // The demoted grep lane: present, named, and off until asked for.
    await expect(dock.locator('[data-kbc-usages-chip="mentions"]')).toBeVisible();

    // The buffer still owns the keyboard — the dock never took focus, so the
    // vim keymap sees keys immediately. (This is what the old Esc-then-`gg`
    // tail was really pinning; there is no popup left to dismiss.)
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
