import { expect, test } from "@playwright/test";
import { CALLER_FILE, KNOWN_FILE, KNOWN_SYMBOL, TEXT_NEEDLE } from "./fixture-repo";
import { BASE, REPO_NAME } from "./helpers";

/// Wave E ("kb-code v2 — The Operable Reader", E1+E2) end to end: URL-driven
/// two-pane splits, the `Ctrl-w v`/`Ctrl-w q` vim gestures, the tree's
/// Shift+Enter, and the working-set strip's chips + `]f` cycling. Runs
/// against the SAME fixture repo every other e2e spec shares — additive
/// only, no fixture changes needed (`KNOWN_FILE`/`CALLER_FILE` are already
/// two distinct root-level files with distinguishable content).

const PANES = ".kbc-reader__pane";

/// Blur out of whichever pane currently holds focus back to the tree
/// (`Ctrl-w h` — `pane.focus-prev`/`handlePaneFocus(pane, "prev")`), so a
/// following `j`/`k`/`Enter` reaches the window-level tree-navigation
/// handler rather than the vim keymap.
///
/// V70-H1 — the real bug this test caught: NONE of the registry's five
/// `Ctrl-w` pane commands (`pane.focus-prev/-next/-cycle`, `pane.split`,
/// `pane.close`) had a registered central handler in `Reader.tsx`.
/// `CommandRoot.onKey` consumes `Ctrl-w` as a pending chord regardless
/// (it isn't a bare token, so the read-only-buffer carve-out never
/// applies to it) — so the second key of the chord resolves to a REAL
/// command that has no owning handler, and is swallowed (`if (!handler)
/// return; // nothing owns it here`) before it ever reaches the vim
/// reducer's own `onPaneFocus` callback. Fixed by wiring all five to
/// `Reader.tsx`'s existing `handlePaneFocus`/`handleSplitSelf`/
/// `handleClosePane` functions — this helper was never the problem, it's
/// back to exactly the keyboard gesture a real user presses.
async function escapeToTree(page: import("@playwright/test").Page) {
  await page.locator(".kbc-codeview .cm-line").first().click();
  await page.keyboard.press("Control+w");
  await page.keyboard.press("h");
}

test.describe("Wave E — two-pane splits", () => {
  test("a hard-navigated ?pane2= URL renders two panes, each showing its own file", async ({ page }) => {
    const pane2Value = encodeURIComponent(`${CALLER_FILE}@:`);
    await page.goto(`${BASE}/r/${REPO_NAME}/${KNOWN_FILE}?pane2=${pane2Value}`);

    const panes = page.locator(PANES);
    await expect(panes).toHaveCount(2);
    await expect(panes.nth(0)).toContainText(TEXT_NEEDLE, { timeout: 10_000 });
    await expect(panes.nth(1)).toContainText("caller_one", { timeout: 10_000 });
  });

  test("Ctrl-w v splits with self; Ctrl-w q closes the focused pane and the URL drops pane2=", async ({ page }) => {
    await page.goto(`${BASE}/r/${REPO_NAME}`);
    await page.locator(".kbc-tree__row", { hasText: KNOWN_FILE }).click();
    await expect(page.locator(".kbc-codeview")).toContainText(KNOWN_SYMBOL, { timeout: 10_000 });
    await page.locator(".kbc-codeview .cm-line").first().click();

    await page.keyboard.press("Control+w");
    await page.keyboard.press("v");

    await expect(page).toHaveURL(/[?&]pane2=/);
    const panes = page.locator(PANES);
    await expect(panes).toHaveCount(2);
    // "Split with self" — pane2 shows the SAME file pane1 was on.
    await expect(panes.nth(1)).toContainText(KNOWN_SYMBOL, { timeout: 10_000 });

    // Wait for real DOM focus to have actually landed in pane2 (the "split
    // with self" gesture focuses the new pane) before closing it, so the
    // NEXT Ctrl-w q is unambiguously "close the pane that was just opened."
    await page.waitForFunction(() => {
      const els = document.querySelectorAll(".kbc-reader__pane");
      return els[1] != null && els[1].contains(document.activeElement);
    });

    await page.keyboard.press("Control+w");
    await page.keyboard.press("q");

    await expect(page).not.toHaveURL(/[?&]pane2=/);
    await expect(panes).toHaveCount(1);
    // The single remaining pane is still pane1's own file (closing pane2
    // must never touch pane1's content).
    await expect(panes.nth(0)).toContainText(KNOWN_SYMBOL, { timeout: 10_000 });
  });

  test("Shift+Enter in the tree opens the focused row into pane2", async ({ page }) => {
    await page.goto(`${BASE}/r/${REPO_NAME}/${KNOWN_FILE}`);
    await expect(page.locator(".kbc-codeview")).toContainText(KNOWN_SYMBOL, { timeout: 10_000 });
    await escapeToTree(page);
    // V70-H1 — pin the postcondition `escapeToTree`'s name promises: the
    // tree is genuinely visible and `keyboardRegion` (`Reader.tsx`, gates
    // `CommandRoot`'s scope to `"tree"`) has flipped before any tree-scope
    // key is exercised below.
    await expect(page.locator("[data-kbc-tree]")).toBeVisible({ timeout: 5_000 });

    // The tree's row cursor starts at index 0. `sortTreeEntries` always
    // ranks directories before files (`lib/tree.ts`), and DCB-W2.B.R fix 9
    // added exactly ONE new top-level directory (`ambig/`, `doclens-
    // fixture.ts`'s `seedAmbiguityDemo`) — so row 0 is that directory and
    // row 1 is normally "caller.rs", alphabetically first among the
    // fixture's root FILES (see `fixture-repo.ts`). But this daemon+repo
    // is SHARED across the whole suite (specs run alphabetically against
    // ONE fixture, per `checkout-dirty.spec.ts`'s own header doc), so the
    // exact row index is not something this spec should hard-code — a
    // sibling spec's own fixture state can shift it by rows this file has
    // no visibility into. Poll: check the target row's focus marker, and
    // if it isn't there yet, press `j` and try again — self-adapting to
    // however many rows away "caller.rs" actually is, capped by the
    // timeout below (also closes the `useImperativeHandle`/`focusedIndex`
    // commit race a single unconditional `j` could lose: `activateFocused`
    // only reflects the LATEST committed render).
    const callerRow = page.locator(".kbc-tree__row", { hasText: CALLER_FILE });
    await expect
      .poll(
        async () => {
          const isFocused = await callerRow.evaluate((el) =>
            el.classList.contains("kbc-tree__row--focused"),
          );
          if (!isFocused) await page.keyboard.press("j");
          return isFocused;
        },
        { timeout: 8_000, intervals: [150] },
      )
      .toBe(true);
    await page.keyboard.press("Shift+Enter");

    await expect(page).toHaveURL(new RegExp(`pane2=${encodeURIComponent(CALLER_FILE)}`));
    const panes = page.locator(PANES);
    await expect(panes).toHaveCount(2);
    await expect(panes.nth(1)).toContainText("caller_one", { timeout: 10_000 });
    // pane1 is untouched — Shift+Enter never repoints the primary file.
    await expect(panes.nth(0)).toContainText(KNOWN_SYMBOL, { timeout: 10_000 });
  });
});

test.describe("Wave E — working-set strip", () => {
  test("chips appear after opening two files; clicking a chip switches the focused pane; ]f cycles", async ({
    page,
  }) => {
    await page.goto(`${BASE}/r/${REPO_NAME}`);
    await page.locator(".kbc-tree__row", { hasText: KNOWN_FILE }).click();
    await expect(page.locator(".kbc-codeview")).toContainText(KNOWN_SYMBOL, { timeout: 10_000 });

    // Open a SECOND file into the same (focused) pane — the working set
    // now has two members: lib.rs (touched first), then caller.rs.
    await escapeToTree(page);
    await page.locator(".kbc-tree__row", { hasText: CALLER_FILE }).click();
    await expect(page.locator(".kbc-codeview")).toContainText("caller_one", { timeout: 10_000 });

    const chips = page.locator("[data-kbc-ws-chip]");
    await expect(chips).toHaveCount(2);
    const libChip = page.locator(`[data-kbc-ws-chip="${KNOWN_FILE}"]`);
    const callerChip = page.locator(`[data-kbc-ws-chip="${CALLER_FILE}"]`);
    await expect(libChip).toBeVisible();
    await expect(callerChip).toBeVisible();

    // Clicking the lib.rs chip switches the focused pane's file back to it.
    await libChip.click();
    await expect(page).toHaveURL(new RegExp(`/r/${REPO_NAME}/${KNOWN_FILE}(\\?|$)`));
    await expect(page.locator(".kbc-codeview")).toContainText(KNOWN_SYMBOL, { timeout: 10_000 });

    // ]f cycles the focused pane forward through the working set's stable
    // order (lib.rs, then caller.rs) — from lib.rs, the next entry is
    // caller.rs.
    await page.locator(".kbc-codeview .cm-line").first().click();
    await page.keyboard.press("]");
    await page.keyboard.press("f");
    await expect(page).toHaveURL(new RegExp(`/r/${REPO_NAME}/${CALLER_FILE}(\\?|$)`));
    await expect(page.locator(".kbc-codeview")).toContainText("caller_one", { timeout: 10_000 });
  });
});
