import { expect, test, type Page } from "@playwright/test";
import { KNOWN_FILE, KNOWN_SYMBOL } from "./fixture-repo";
import { BASE, REPO_NAME } from "./helpers";

/// V70-A5 — the day-one script, as an executable exit criterion (design D24):
///
///   "A repo opens to the reader with a working-tree tree and git status, the
///    last file or the Home card, the rail on All, no drawer, dark theme, vim
///    preset. The only teaching chrome above the fold is a Space hint and ?.
///    The first gd shows a one-time dismissible coach-mark for the Ramp; the
///    first mouse action with a key shows one learn-mode toast. Nothing else
///    is offered until asked."
///
/// Every assertion below is one clause of that paragraph. It runs on a FRESH
/// PROFILE — Playwright gives each test its own context, so `localStorage` is
/// empty and the one-time affordances are genuinely first-run. That is the
/// whole point: the things this checks are precisely the ones that stop being
/// checkable the second time you look at them by hand.

async function freshReader(page: Page) {
  await page.goto(`${BASE}/r/${REPO_NAME}`);
  await expect(page.locator(".kbc-tree__row").first()).toBeVisible({ timeout: 15_000 });
}

async function openFixtureFile(page: Page) {
  await page.locator(".kbc-tree__row", { hasText: KNOWN_FILE }).click();
  await expect(page.locator(".kbc-codeview")).toContainText(KNOWN_SYMBOL, { timeout: 15_000 });
  await page.locator(".kbc-codeview .cm-line").first().click();
}

test.describe("day one", () => {
  test("a fresh profile lands on the reader: tree, dark theme, vim preset, no drawer", async ({
    page,
  }) => {
    await freshReader(page);

    // …the tree.
    await expect(page.locator("[data-region='dock']")).toBeVisible();
    // …dark theme. `applyTheme(loadTheme())` runs pre-paint, and an unset
    // pref is `"dark"`.
    await expect(page.locator("html")).toHaveAttribute("data-theme", "dark");
    // …no drawer. The region is always PRESENT (§P1: "regions collapse to a
    // badged stripe and never vanish", and the landmark golden depends on
    // that) — what a cold start promises is that it is COLLAPSED.
    await expect(page.locator("[data-region='drawer']")).toHaveAttribute(
      "data-desk-drawer-collapsed",
      "1",
    );
    // …vim preset, which is what `?` renders. `g d` is the vim column's
    // goto-definition; if the default preset ever flipped, this row would
    // read `F12` instead.
    await page.keyboard.press("?");
    const sheet = page.locator("[data-kbc-kbdhelp]");
    await expect(sheet).toBeVisible();
    await expect(sheet.locator("[data-kbc-preset]")).toHaveValue("vim");
    await expect(
      sheet.locator("[data-kbc-cmd='reader.goto-definition'] kbd"),
    ).toHaveText("g d");
    await page.keyboard.press("Escape");
    await expect(sheet).toBeHidden();
  });

  test("the only teaching chrome above the fold is the Space hint and ?", async ({ page }) => {
    await freshReader(page);

    // The chip is there…
    const hint = page.locator("[data-kbc-spacehint]");
    await expect(hint).toBeVisible();
    await expect(hint).toContainText("?");

    // …and nothing else is. No tour, no modal, no coach-mark, no toast: D24's
    // "nothing else is offered until asked" is an assertion about ABSENCE,
    // which is the clause a hand-check always forgets.
    await expect(page.locator("[data-kbc-coachmark]")).toHaveCount(0);
    await expect(page.locator("[data-kbc-kbdhelp]")).toHaveCount(0);
    await expect(page.locator(".kbc-toast")).toHaveCount(0);
  });

  test("the first gd shows a one-time, dismissible Ramp coach-mark", async ({ page }) => {
    await freshReader(page);
    await openFixtureFile(page);

    // Land the cursor on the known symbol, then `g d`.
    await page.locator(".kbc-codeview .cm-content").getByText(KNOWN_SYMBOL).first().click();
    await page.keyboard.press("g");
    await page.keyboard.press("d");

    const coach = page.locator("[data-kbc-coachmark]");
    await expect(coach).toBeVisible({ timeout: 10_000 });
    // It teaches the Ramp — the commitment gradient `gd` is the far end of.
    await expect(coach).toContainText("K");
    await expect(coach).toContainText("Shift-Enter");

    await coach.locator("[data-kbc-coachmark-close]").click();
    await expect(coach).toBeHidden();

    // ONE time, ever: a second `gd` — even after a reload — must not bring it
    // back. The counter is browser-local, so a reload is the real test.
    await page.reload();
    await openFixtureFile(page);
    await page.locator(".kbc-codeview .cm-content").getByText(KNOWN_SYMBOL).first().click();
    await page.keyboard.press("g");
    await page.keyboard.press("d");
    await page.waitForTimeout(500);
    await expect(page.locator("[data-kbc-coachmark]")).toHaveCount(0);
  });

  test("the first mouse click on a keyed control earns exactly one learn toast", async ({
    page,
  }) => {
    await freshReader(page);

    // The drawer toggle carries `data-cmd="desk.toggle.drawer"`, which the
    // registry binds to `Space d`.
    const toggle = page.locator("[data-cmd='desk.toggle.drawer']").first();
    await expect(toggle).toBeVisible();
    await toggle.click();
    const toast = page.locator(".kbc-toast", { hasText: "Space d" });
    await expect(toast).toBeVisible({ timeout: 5_000 });

    // Once per command, ever. Clicking it again teaches nothing — a teaching
    // aid that keeps teaching after you have learned is just noise.
    await page.locator(".kbc-toast").first().waitFor({ state: "hidden", timeout: 15_000 });
    await toggle.click();
    await page.waitForTimeout(600);
    await expect(page.locator(".kbc-toast", { hasText: "Space d" })).toHaveCount(0);
  });
});

test.describe("the doors that now open everywhere", () => {
  test("? is scoped, and works on a route that used to be keyboard-inert", async ({ page }) => {
    // `~reviews` was one of the sixteen routes where `?` did nothing at all.
    await page.goto(`${BASE}/r/${REPO_NAME}/~reviews`);
    await page.locator("body").click();
    await page.keyboard.press("?");
    await expect(page.locator("[data-kbc-kbdhelp]")).toBeVisible({ timeout: 10_000 });
    // The sheet always names the printable twin, so the keyboard surface has
    // an off-screen home too.
    await expect(page.locator(".kbc-kbdhelp__foot")).toContainText("kb-code commands cheatsheet");
  });

  test("the leader raises which-key, and Escape cancels the chord without dismissing", async ({
    page,
  }) => {
    await freshReader(page);
    await page.locator("body").click();

    await page.keyboard.press("Space");
    // Passive observer, after ~400ms — never dispatches, never steals focus.
    const which = page.locator("[data-kbc-whichkey]");
    await expect(which).toBeVisible({ timeout: 5_000 });
    await expect(which.locator("[data-kbc-whichkey-key='g']")).toBeVisible();
    await expect(which.locator("[data-kbc-whichkey-key='d']")).toBeVisible();

    // Escape collapses the chord — and does NOT also open/close anything
    // else. One keystroke, one job.
    await page.keyboard.press("Escape");
    await expect(which).toBeHidden();
    await expect(page.locator("[data-kbc-kbdhelp]")).toHaveCount(0);
  });

  test("the palette runs commands, shows their keys, and is honest about the rest", async ({
    page,
  }) => {
    await freshReader(page);
    await page.locator("body").click();

    // `:` opens the box already in command mode.
    await page.keyboard.press(":");
    const box = page.locator(".kbc-omnibox");
    await expect(box).toBeVisible({ timeout: 5_000 });
    await expect(box.locator("[data-kbc-cmdmode]")).toBeVisible();
    // The active scope is stated — which rows you are looking at is a fact
    // about where you are.
    await expect(box.locator("[data-kbc-cmdscope]")).toBeVisible();

    await page.keyboard.type("inbox");
    const row = box.locator("[data-kbc-cmdrow='nav.inbox']");
    await expect(row).toBeVisible();
    // The key column is the palette's teaching job.
    await expect(row.locator("kbd")).toHaveText("Space g i");
    // …and it is the row Enter would run: the cursor starts on the top row,
    // so asserting the selection here is what makes the Enter below a test of
    // execution rather than of luck.
    await expect(row).toHaveAttribute("aria-selected", "true");

    // Unavailable rows are hidden behind a counted toggle, never silently
    // dropped.
    const toggle = box.locator("[data-kbc-cmdtoggle]");
    await expect(toggle).toContainText("Show unavailable");

    await page.keyboard.press("Enter");
    await expect(page).toHaveURL(/\/~inbox$/, { timeout: 10_000 });
  });

  test("V70-K1 — the Space leader still opens from inside the CM6 buffer", async ({ page }) => {
    // Before this fix, `CommandRoot`'s guard withheld EVERY bare key the
    // instant the buffer held genuine DOM focus — not just the ones the vim
    // layer (`vimKeys.ts`) actually reacts to. `Space` isn't one of them
    // (`vimKeysReducer` has no case for it at all), so the entire
    // `Space`-leader family (`desk.toggle.drawer` included) was
    // structurally dead the moment a file was open — "focus follows the
    // file" (A3) puts focus exactly there. `openFixtureFile` genuinely
    // focuses the buffer (unlike this describe block's other tests, which
    // click `body`), so this is the one case those didn't cover.
    await freshReader(page);
    await openFixtureFile(page);

    const drawer = page.locator("[data-region='drawer']");
    await expect(drawer).toHaveAttribute("data-desk-drawer-collapsed", "1");

    await page.keyboard.press("Space");
    await page.keyboard.press("d");
    await expect(drawer).toHaveAttribute("data-desk-drawer-collapsed", "0");

    // And back — `Space d` is a toggle, run from the buffer both times.
    await page.keyboard.press("Space");
    await page.keyboard.press("d");
    await expect(drawer).toHaveAttribute("data-desk-drawer-collapsed", "1");
  });

  test("a ?cmd= deep link auto-runs a read-only command", async ({ page }) => {
    // `nav.home` is `mutation: none` + `side_effect: none`, so it may run
    // straight from the URL; anything that writes would pre-fill the palette
    // instead (`deepLinkDisposition`, unit-pinned).
    await page.goto(`${BASE}/r/${REPO_NAME}/~reviews?cmd=nav.inbox`);
    await expect(page).toHaveURL(/\/~inbox$/, { timeout: 10_000 });
    // The param is stripped, so a refresh does not re-run it.
    expect(new URL(page.url()).searchParams.get("cmd")).toBeNull();
  });
});
