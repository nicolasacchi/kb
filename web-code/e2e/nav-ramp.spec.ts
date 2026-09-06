import { expect, test, type Page } from "@playwright/test";
import { KNOWN_FILE, KNOWN_SYMBOL } from "./fixture-repo";
import { BASE, REPO_NAME } from "./helpers";

/// V70-A6 — the Ramp (§P7): ONE commitment gradient on every result row, and
/// the trail linkage that makes "open in a new tab" stop being a dead end.
///
/// The rungs, and what each one is pinned to here:
///
///   Enter        open in the focused pane        (pre-A6 behaviour, unchanged)
///   Shift-Enter  the OTHER pane, via `?pane2=`   (root CLAUDE.md #30 grammar)
///   Ctrl-Enter   a new browser TAB, trail-linked (`trail=`/`step=`/`via=`)
///   o            the same tab rung, one key
///   O            a new browser WINDOW, same link
///   u            in a trail-linked tab: walk back to the origin as a PUSH,
///                so the destination stays exactly one Back away
///
/// The tree is the surface under test because it is the one that always has a
/// focused row without a file being open — and because a tree row was, before
/// this unit, one of six surfaces where Cmd-click and middle-click silently
/// did nothing at all (the navigation-history recon's own table).

/// The repo root: no file open, so the reader publishes the `tree` scope.
/// `sortTreeEntries` always ranks directories before files (`lib/tree.ts`),
/// and DCB-W2.B.R fix 9 added a top-level directory (`ambig/`,
/// `doclens-fixture.ts`'s `seedAmbiguityDemo`); V72-I2 added two more
/// (`app/`, `config/` — the Rails fixture app) — so the tree's row cursor
/// starts on a DIRECTORY, not a file (`split.spec.ts`'s own "Shift+Enter
/// in the tree" test hit the same thing). The helper steps down to the
/// first root FILE — the row `rampFocusedTreeRow` (`Reader.tsx`) needs,
/// since a directory has nowhere else to be opened. The assertion checks
/// the row's KIND and not its name, which is what lets a root file be
/// added without touching this helper.
async function openTreeScope(page: Page) {
  await page.goto(`${BASE}/r/${REPO_NAME}`);
  await expect(page.locator(".kbc-tree__row").first()).toBeVisible({ timeout: 10_000 });
  // V72-I2 — was a single `j`, which encoded "exactly one root DIRECTORY
  // sorts above the first root file". That number is a property of the
  // fixture, not of the Ramp, and it has now changed twice (`ambig/`, then
  // this unit's `app/` + `config/` Rails tree). Step until the cursor is on
  // a file, bounded, so the next lane that adds a root directory does not
  // have to find this helper by breaking six tests.
  const focused = page.locator(".kbc-tree__row--focused");
  for (let i = 0; i < 12; i++) {
    await page.keyboard.press("j");
    if ((await focused.getAttribute("data-kbc-kind")) === "file") break;
  }
  await expect(focused, "no root FILE row within 12 `j` presses").toHaveAttribute(
    "data-kbc-kind",
    "file",
  );
}

/// Focus the row for `file` by clicking it (which also sets the tree's own
/// row cursor), then return to a state where the WINDOW host owns bare keys:
/// clicking a row opens the file and focus follows into the buffer, so the
/// keyboard rungs are exercised from the tree by re-focusing the tree's
/// filter input's sibling — in practice, by pressing the tree's own `k`/`j`
/// after blurring the buffer.
async function focusTreeRow(page: Page, file: string) {
  const row = page.locator(".kbc-tree__row", { hasText: file });
  await expect(row).toBeVisible({ timeout: 10_000 });
  return row;
}

const TRAIL_QUERY = /[?&]trail=[0-9a-f]+&step=\d+&via=\w+/;

test.describe("the Ramp (§P7)", () => {
  test("Ctrl-Enter on the tree's focused row opens a TRAIL-LINKED new tab", async ({ page, context }) => {
    await openTreeScope(page);

    const [popup] = await Promise.all([
      context.waitForEvent("page"),
      page.keyboard.press("Control+Enter"),
    ]);
    await popup.waitForLoadState("domcontentloaded");

    // The destination carries the whole triple: which trail, which hop, and
    // the typed edge that was traversed. A tree open is `via=tree`.
    expect(popup.url()).toMatch(TRAIL_QUERY);
    expect(popup.url()).toContain("via=tree");
    expect(popup.url()).toContain(`/r/${REPO_NAME}/`);

    // The ORIGIN tab is untouched: opening elsewhere is not a navigation here.
    expect(page.url()).toBe(`${BASE}/r/${REPO_NAME}`);
    await popup.close();
  });

  test("`o` is the same rung, one key", async ({ page, context }) => {
    await openTreeScope(page);
    const [popup] = await Promise.all([context.waitForEvent("page"), page.keyboard.press("o")]);
    await popup.waitForLoadState("domcontentloaded");
    expect(popup.url()).toMatch(TRAIL_QUERY);
    await popup.close();
  });

  test("`O` opens a new WINDOW with the same trail link", async ({ page, context }) => {
    await openTreeScope(page);
    const [popup] = await Promise.all([
      context.waitForEvent("page"),
      // Shift-o — the registry's `O`, folded into the character exactly the
      // way `dispatch.ts`'s `tokenOf` canonicalises it.
      page.keyboard.press("Shift+O"),
    ]);
    await popup.waitForLoadState("domcontentloaded");
    expect(popup.url()).toMatch(TRAIL_QUERY);
    await popup.close();
  });

  test("Shift-Enter opens the OTHER pane through the ?pane2= grammar", async ({ page }) => {
    // Open a file first so there IS a pane 1 for pane 2 to be beside.
    await openTreeScope(page);
    await (await focusTreeRow(page, KNOWN_FILE)).click();
    await expect(page.locator(".kbc-codeview")).toContainText(KNOWN_SYMBOL, { timeout: 10_000 });

    // Shift+click a DIFFERENT row — the tree's own pane-2 gesture, which the
    // Ramp deliberately leaves alone (`tree.open-pane2` keeps the tree's row).
    await (await focusTreeRow(page, "caller.rs")).click({ modifiers: ["Shift"] });
    await expect(page).toHaveURL(/[?&]pane2=/, { timeout: 5_000 });
    await expect(page.locator('[data-region="pane-2"]')).toBeVisible();
  });

  test("a trail-linked tab renders the origin chip, and `u` pushes the origin", async ({
    page,
    context,
  }) => {
    await openTreeScope(page);
    const [popup] = await Promise.all([
      context.waitForEvent("page"),
      page.keyboard.press("Control+Enter"),
    ]);
    await popup.waitForLoadState("domcontentloaded");
    const destination = popup.url();

    // The chip names where the hop came FROM. It resolves either from the
    // sessionStorage copy a `window.open`-ed tab inherits, or from the
    // `kbc-tabs` BroadcastChannel hand-off — both are bounded, and where
    // neither works no chip renders at all (never a dead one).
    const chip = popup.locator("[data-kbc-trail-chip]");
    await expect(chip).toBeVisible({ timeout: 10_000 });
    await expect(chip).toContainText("from");

    // `u` walks back to the origin as a PUSH — the destination stays exactly
    // one Back away, and nothing tries to focus another tab. This is the ONE
    // Ramp rung that has to work from INSIDE the focused buffer (A3's "focus
    // follows the file" lands DOM focus there the instant this tab's file
    // renders), which is why `u` is also the vim layer's own bare-key arm
    // (`vimKeys.ts`'s `cb-nav-back`), not just `CommandRoot`'s central row.
    await popup.keyboard.press("u");
    await expect(popup).toHaveURL(new RegExp(`/r/${REPO_NAME}$`), { timeout: 5_000 });
    await popup.goBack();
    await expect(popup).toHaveURL(destination, { timeout: 5_000 });
    await popup.close();
  });

  test("Ctrl-click on a tree row opens a trail-linked tab (it used to do nothing)", async ({
    page,
    context,
  }) => {
    await openTreeScope(page);
    const row = await focusTreeRow(page, KNOWN_FILE);
    const [popup] = await Promise.all([
      context.waitForEvent("page"),
      row.click({ modifiers: ["ControlOrMeta"] }),
    ]);
    await popup.waitForLoadState("domcontentloaded");
    expect(popup.url()).toContain(`/r/${REPO_NAME}/${KNOWN_FILE}`);
    expect(popup.url()).toMatch(TRAIL_QUERY);
    await popup.close();
  });
});

test.describe("per-pane history (§P7 · Pane Relief)", () => {
  test("the pane header carries back/forward arrows with a count and a preview", async ({ page }) => {
    await page.goto(`${BASE}/r/${REPO_NAME}`);
    await (await focusTreeRow(page, KNOWN_FILE)).click();
    await expect(page.locator(".kbc-codeview")).toContainText(KNOWN_SYMBOL, { timeout: 10_000 });
    await (await focusTreeRow(page, "caller.rs")).click();
    await expect(page.locator(".kbc-codeview")).toContainText(KNOWN_SYMBOL, { timeout: 10_000 });

    const back = page.locator('[data-kbc-panehist="1"] [data-kbc-panehist-back]');
    await expect(back).toBeEnabled({ timeout: 5_000 });
    // The count badge is the whole point: Back stops being a slot machine.
    await expect(back.locator(".kbc-panehist__count")).toHaveText(/\d+/);
    // The hover preview says WHERE you would land, and by which edge.
    await expect(back).toHaveAttribute("title", /Back — .*:\d+/);

    await back.click();
    await expect(page).toHaveURL(new RegExp(`/r/${REPO_NAME}/${KNOWN_FILE}`), { timeout: 5_000 });
  });
});
