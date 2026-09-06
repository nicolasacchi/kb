import { expect, test, type Page } from "@playwright/test";
import { LOCAL_TARGET_FN, RESOLVER_FILE } from "./fixture-repo";
import { BASE, REPO_NAME } from "./helpers";

/// V70-A6 — the identifier hover tooltip (§P7's Ramp scent, hover/1's first
/// SPA consumer, `editor/hoverTooltip.ts`).
///
/// Three of the module's own four rules are user-visible in a browser and
/// pinned here (the fourth — never a fetch per mouse move — is a caching
/// detail exercised by the unit test, `src/editor/hoverTooltip.test.ts`):
///
///   1. hovering an identifier ≥500ms surfaces a resolved tooltip
///   2. a token that resolves to nothing renders the HONEST empty, not a
///      silently vanished box (Sourcegraph's own tracked failure)
///   3. Ctrl/Cmd-click IS `gd` (the modifier is the WHOLE discrimination —
///      a bare click must never navigate out of the file mid-read)

async function openResolverFile(page: Page) {
  await page.goto(`${BASE}/r/${REPO_NAME}`);
  await page.locator(".kbc-tree__row", { hasText: RESOLVER_FILE }).click();
  await expect(page.locator(".kbc-codeview")).toContainText(LOCAL_TARGET_FN, { timeout: 10_000 });
}

/// `local_target` appears twice — its declaration and its call site inside
/// `calls_local_target`. `.last()` is the call site, same convention as
/// `peek-inline.spec.ts`'s `clickCallSite`.
function callSite(page: Page) {
  return page.locator(".kbc-codeview").getByText(LOCAL_TARGET_FN, { exact: true }).last();
}

test.describe("identifier hover tooltip (hover/1)", () => {
  test("hovering an identifier surfaces the resolved tooltip", async ({ page }) => {
    await openResolverFile(page);
    await callSite(page).hover();

    const tip = page.locator("[data-kbc-hovertip]");
    await expect(tip).toBeVisible({ timeout: 5_000 });
    await expect(tip).toContainText(LOCAL_TARGET_FN);
    // The keys hint only renders on a resolved (non-empty) card — its
    // presence here doubles as proof this was NOT the honest-empty branch.
    await expect(tip.locator(".kbc-hovertip__keys")).toContainText(
      "Ctrl-click to go to the definition",
    );
  });

  test("a token that resolves to nothing renders the honest empty, never a silent absence", async ({
    page,
  }) => {
    await openResolverFile(page);
    // `local_target`'s body is a bare numeric literal (`42`, line 3) — a
    // real word by the editor's own word-boundary rule, but not a code
    // symbol anything in the repo resolves to.
    await page.locator(".kbc-codeview").getByText("42", { exact: true }).hover();

    const tip = page.locator("[data-kbc-hovertip]");
    await expect(tip).toBeVisible({ timeout: 5_000 });
    await expect(tip).toContainText("nothing in this repo resolves this identifier");
    // The honest-empty branch never appends the keys hint — there is
    // nothing here to Ctrl-click to.
    await expect(tip.locator(".kbc-hovertip__keys")).toHaveCount(0);
  });

  test("Ctrl/Cmd-click goes to the definition — it IS `gd`", async ({ page }) => {
    await openResolverFile(page);
    await callSite(page).click({ modifiers: ["ControlOrMeta"] });

    // `local_target` has exactly one candidate, so `gd`'s own behaviour
    // (`peek-inline.spec.ts`) opens it IN PLACE as an inline peek — proof
    // the click reached the SAME `onGotoDef` handler the `gd` key does,
    // not a private navigation of its own.
    const peek = page.locator("[data-kbc-inpeek]");
    await expect(peek).toBeVisible({ timeout: 10_000 });
    await expect(peek.locator("[data-kbc-inpeek-body]")).toContainText(LOCAL_TARGET_FN, {
      timeout: 10_000,
    });
  });

  test("a plain click is never a navigation — the modifier is the whole discrimination", async ({
    page,
  }) => {
    await openResolverFile(page);
    const before = page.url();
    await callSite(page).click();

    // No peek, no URL change: a bare click only ever places the caret.
    await expect(page.locator("[data-kbc-inpeek]")).toHaveCount(0);
    expect(page.url()).toBe(before);
  });
});
