import { expect, test } from "@playwright/test";
import {
  CALLER_FILE,
  KNOWN_SYMBOL,
  LOCAL_TARGET_DOC,
  LOCAL_TARGET_FN,
  RESOLVER_FILE,
} from "./fixture-repo";
import { BASE, REPO_NAME } from "./helpers";

/// B3's position-based `/api/resolve` — `gd`'s new PRIMARY path (the
/// name-based `/api/defs` lookup B1 shipped now fires ONLY as a fallback
/// when resolve itself fails) and `K`'s new "what AND why" provenance hover
/// card, replacing B1's plain best-def-hit summary. `resolver.rs` (added
/// additively to `fixture-repo.ts`, alongside `KNOWN_FILE`/`CALLER_FILE`) is
/// a THIRD file carrying a doc-commented `local_target()` plus its own call
/// site a few lines below — a clean file-local single-candidate case,
/// distinct from `KNOWN_SYMBOL`'s cross-file one.

async function openResolverFile(page: import("@playwright/test").Page) {
  await page.goto(`${BASE}/r/${REPO_NAME}`);
  await page.locator(".kbc-tree__row", { hasText: RESOLVER_FILE }).click();
  await expect(page.locator(".kbc-codeview")).toContainText(LOCAL_TARGET_FN, { timeout: 10_000 });
}

/// `local_target` appears twice in `RESOLVER_FILE` — its own declaration
/// (first) and its call site a few lines below (last). `.last()` picks the
/// CALL site deterministically, same exact-text-node reasoning
/// `clickable.spec.ts`'s own `clickKnownSymbolUsage` documents (tree-sitter's
/// bundled `highlights.scm` tags a call's callee as its own `@function`
/// node, so it renders as its own element).
async function clickLocalTargetCallSite(page: import("@playwright/test").Page) {
  await page.locator(".kbc-codeview").getByText(LOCAL_TARGET_FN, { exact: true }).last().click();
}

test.describe("resolve (B3)", () => {
  test("gd on the call site opens the file-local definition in place — no peek panel, no navigation", async ({
    page,
  }) => {
    await openResolverFile(page);
    const before = page.url();
    await clickLocalTargetCallSite(page);

    await page.keyboard.press("g");
    await page.keyboard.press("d");

    // `local_target` is defined exactly once, in THIS file — resolve's
    // file-local tier answers with a single candidate, so `gd` no longer
    // navigates (V70-A6 §P7): it opens the definition as an INLINE peek
    // under the caret line (same "skip the OLD panel" outcome B1's
    // name-based path used to reach by navigating, now reached by staying
    // put — `peek-inline.spec.ts` is the mechanism's own suite).
    const peek = page.locator("[data-kbc-inpeek]");
    await expect(peek).toBeVisible({ timeout: 10_000 });
    await expect(peek.locator("[data-kbc-inpeek-body]")).toContainText(LOCAL_TARGET_FN, { timeout: 10_000 });
    expect(new URL(page.url()).pathname).toBe(new URL(before).pathname);
    await expect(page.locator("[data-kbc-peek]")).toHaveCount(0);
  });

  test("K on the call site shows the provenance hover card with the signature and doc excerpt", async ({ page }) => {
    await openResolverFile(page);
    await clickLocalTargetCallSite(page);

    await page.keyboard.press("K");

    const panel = page.locator("[data-kbc-peek]");
    await expect(panel).toBeVisible();
    await expect(panel).toHaveAttribute("data-kbc-peek-mode", "hover");
    const card = panel.locator("[data-kbc-peek-card]");
    await expect(card).toBeVisible({ timeout: 10_000 });

    // The signature (`fn local_target() -> i32 `, whitespace-collapsed —
    // see `extract::build_signature`'s doc) and the doc excerpt (comment
    // markers stripped — `extract::capture_doc`'s doc) both render.
    await expect(card).toContainText(LOCAL_TARGET_FN);
    await expect(card).toContainText(LOCAL_TARGET_DOC);

    await page.keyboard.press("Escape");
    await expect(panel).toHaveCount(0);
  });

  test("gd on the cross-file KNOWN_SYMBOL usage in caller.rs opens lib.rs in place, same repo", async ({
    page,
  }) => {
    await page.goto(`${BASE}/r/${REPO_NAME}`);
    await page.locator(".kbc-tree__row", { hasText: CALLER_FILE }).click();
    await expect(page.locator(".kbc-codeview")).toContainText(KNOWN_SYMBOL, { timeout: 10_000 });
    const before = page.url();
    await page.locator(".kbc-codeview").getByText(KNOWN_SYMBOL, { exact: true }).first().click();

    await page.keyboard.press("g");
    await page.keyboard.press("d");

    // Whichever path answers — resolve's same-repo tags-approx tier (a
    // single symbols-table match, since `KNOWN_SYMBOL` is declared exactly
    // once repo-wide) or, if resolve were ever unavailable, B1's own
    // name-based fallback — the OUTCOME is pinned: `lib.rs`, the only place
    // `KNOWN_SYMBOL` is defined. A SAME-repo single candidate stays put and
    // opens it as an inline peek (V70-A6 §P7) rather than navigating —
    // only a CROSS-repo candidate would still navigate (`inlinePeek.ts`'s
    // own doc), and this fixture has one repo.
    const peek = page.locator("[data-kbc-inpeek]");
    await expect(peek).toBeVisible({ timeout: 10_000 });
    await expect(peek.locator("[data-kbc-inpeek-body]")).toContainText(KNOWN_SYMBOL, { timeout: 10_000 });
    expect(new URL(page.url()).pathname).toBe(new URL(before).pathname);
  });
});
