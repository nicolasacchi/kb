import { expect, test, type Page } from "@playwright/test";
import { LOCAL_TARGET_FN, RESOLVER_FILE } from "./fixture-repo";
import { BASE, REPO_NAME } from "./helpers";

/// V70-A6 — the inline peek (§P7's "inline expansion", first slice).
///
/// `gd` on a single candidate used to NAVIGATE, which loses the call site —
/// the exact thing the design says a reader should never have to trade away
/// ("read a call chain in one column"). It now opens the destination's own
/// source as a CM6 block widget under the caret line, nested up to three deep
/// with a breadcrumb, with a `+`/`-` context dial, and `Esc` restores the
/// exact prior scroll.

async function openResolverFile(page: Page) {
  await page.goto(`${BASE}/r/${REPO_NAME}`);
  await page.locator(".kbc-tree__row", { hasText: RESOLVER_FILE }).click();
  await expect(page.locator(".kbc-codeview")).toContainText(LOCAL_TARGET_FN, { timeout: 10_000 });
}

/// `local_target` appears twice — its declaration (first) and its call site
/// (last). `.last()` picks the CALL site, the same way `resolve.spec.ts` does.
async function clickCallSite(page: Page) {
  await page.locator(".kbc-codeview").getByText(LOCAL_TARGET_FN, { exact: true }).last().click();
}

async function pressGd(page: Page) {
  await page.keyboard.press("g");
  await page.keyboard.press("d");
}

test.describe("inline peek", () => {
  test("gd on a single candidate opens it IN PLACE, not by navigating away", async ({ page }) => {
    await openResolverFile(page);
    const before = page.url();
    await clickCallSite(page);
    await pressGd(page);

    const peek = page.locator("[data-kbc-inpeek]");
    await expect(peek).toBeVisible({ timeout: 10_000 });
    // The destination's own source, rendered with the server's highlight
    // spans — not a paraphrase and not a link.
    await expect(peek.locator("[data-kbc-inpeek-body]")).toContainText(LOCAL_TARGET_FN, {
      timeout: 10_000,
    });
    // Still the same file, at the same line: the reader did not move.
    expect(new URL(page.url()).pathname).toBe(new URL(before).pathname);
    await expect(page.locator("[data-kbc-peek]")).toHaveCount(0);
  });

  test("the context dial widens and narrows the excerpt", async ({ page }) => {
    await openResolverFile(page);
    await clickCallSite(page);
    await pressGd(page);
    const body = page.locator("[data-kbc-inpeek-body]");
    await expect(body).toBeVisible({ timeout: 10_000 });

    const lines = () => body.locator("[data-kbc-inpeek-line]").count();
    const start = await lines();
    await page.locator("[data-kbc-inpeek-more]").click();
    // The fixture's `resolver.rs` is short, so the window is clamped by the
    // FILE, not the dial — what is pinned is that the dial never SHRINKS the
    // excerpt and never throws.
    await expect.poll(lines, { timeout: 3_000 }).toBeGreaterThanOrEqual(start);
    await page.locator("[data-kbc-inpeek-less]").click();
    await expect.poll(lines, { timeout: 3_000 }).toBeGreaterThan(0);
  });

  test("nesting shows a breadcrumb, capped at three", async ({ page }) => {
    await openResolverFile(page);
    await clickCallSite(page);
    await pressGd(page);
    const peek = page.locator("[data-kbc-inpeek]");
    await expect(peek).toBeVisible({ timeout: 10_000 });
    await expect(peek).toHaveAttribute("data-kbc-inpeek-depth", "1");

    // Clicking a line inside the excerpt nests one deeper.
    await peek.locator("[data-kbc-inpeek-line]").nth(1).click();
    await expect(peek).toHaveAttribute("data-kbc-inpeek-depth", "2", { timeout: 5_000 });
    const crumbs = peek.locator("[data-kbc-inpeek-crumbs] .kbc-inpeek__crumb");
    await expect(crumbs).toHaveCount(2);

    // Three is the cap: a fourth REPLACES the deepest frame rather than
    // growing the stack (bounded space — Patchworks over Code Bubbles).
    await peek.locator("[data-kbc-inpeek-line]").nth(2).click();
    await expect(peek).toHaveAttribute("data-kbc-inpeek-depth", "3", { timeout: 5_000 });
    await peek.locator("[data-kbc-inpeek-line]").nth(3).click();
    await expect(peek).toHaveAttribute("data-kbc-inpeek-depth", "3");
  });

  test("Esc closes the peek and restores the exact prior scroll", async ({ page }) => {
    await openResolverFile(page);
    await clickCallSite(page);
    const scrollBefore = await page.evaluate(
      () => document.querySelector(".kbc-codeview .cm-scroller")?.scrollTop ?? 0,
    );

    await pressGd(page);
    await expect(page.locator("[data-kbc-inpeek]")).toBeVisible({ timeout: 10_000 });
    await page.keyboard.press("Escape");
    await expect(page.locator("[data-kbc-inpeek]")).toHaveCount(0, { timeout: 5_000 });

    await expect
      .poll(
        () => page.evaluate(() => document.querySelector(".kbc-codeview .cm-scroller")?.scrollTop ?? -1),
        { timeout: 5_000, message: "the buffer's scroll was not restored" },
      )
      .toBe(scrollBefore);
  });

  test("Enter inside a peek promotes it to a pane", async ({ page }) => {
    await openResolverFile(page);
    await clickCallSite(page);
    await pressGd(page);
    await expect(page.locator("[data-kbc-inpeek]")).toBeVisible({ timeout: 10_000 });

    await page.keyboard.press("Enter");
    // §P7: peek → pane promotion is the ONLY thing that creates a pane 2 this
    // way, and it goes through the existing `?pane2=` grammar (#30).
    await expect(page).toHaveURL(/[?&]pane2=/, { timeout: 5_000 });
    await expect(page.locator("[data-kbc-inpeek]")).toHaveCount(0);
  });
});
