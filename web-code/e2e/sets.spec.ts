import { expect, test } from "@playwright/test";
import { CALLER_FILE, KNOWN_FILE } from "./fixture-repo";
import { BASE, REPO_NAME } from "./helpers";

/// Phase E4 ("kb-code v2 — The Operable Reader") end to end: reading sets —
/// create a set with two spans via the SPA, reorder them, step through tour
/// mode, capture a span from the reader's "+ Set" menu, and delete the set
/// via `useConfirm`. Drives `reading_sets.rs` (wired in c9df213e) end to end
/// through the SPA surfaces this phase adds (`routes/Sets.tsx`/
/// `SetDetail.tsx`/`Tour.tsx`, `components/sets/AddToSetMenu.tsx`).
///
/// From-session materialization is NOT covered here: the fixture daemon
/// disables `[transcripts]` entirely (`global-setup.ts`'s `writeConfig`), so
/// `POST /api/sets/from-session` has no session to resolve, and
/// `SessionDiff`'s own page never renders past its loading/error state
/// without a real transcript-backed session to fetch — there is no cheap
/// way to reach a rendered `SessionDiff` + its "Save as reading set" button
/// in this harness. Left unskipped-but-untested would be dishonest, so it's
/// explicitly `test.skip`, not silently absent from the suite.

test.describe("reading sets", () => {
  test("create with two spans, reorder, tour, capture from the reader, delete", async ({ page }) => {
    await page.goto(`${BASE}/r/${REPO_NAME}/~sets`);

    // --- create -----------------------------------------------------------
    await page.locator("[data-kbc-sets-create-name]").fill("the ingest path");
    await page.locator("[data-kbc-sets-create-desc]").fill("how a request flows in");
    await page.locator("[data-kbc-sets-create-submit]").click();

    await expect(page.locator("[data-kbc-sets-row-link]")).toBeVisible();
    await page.locator("[data-kbc-sets-row-link]").click();
    await expect(page).toHaveURL(/~sets\/set_/);

    // --- add two spans via the "Add span" form -----------------------------
    await page.locator("[data-kbc-set-add-path]").fill(KNOWN_FILE);
    await page.locator("[data-kbc-set-add-submit]").click();
    await expect(page.locator("[data-kbc-set-spans] li")).toHaveCount(1);

    await page.locator("[data-kbc-set-add-path]").fill(CALLER_FILE);
    await page.locator("[data-kbc-set-add-submit]").click();
    await expect(page.locator("[data-kbc-set-spans] li")).toHaveCount(2);

    // Appended in insertion order: KNOWN_FILE first, CALLER_FILE second.
    await expect(page.locator("[data-kbc-set-spans] li").nth(0)).toContainText(KNOWN_FILE);
    await expect(page.locator("[data-kbc-set-spans] li").nth(1)).toContainText(CALLER_FILE);

    // --- reorder: move the second row up, so CALLER_FILE now leads ---------
    await page.locator("[data-kbc-set-span-up]").nth(1).click();
    await expect(page.locator("[data-kbc-set-spans] li").nth(0)).toContainText(CALLER_FILE);
    await expect(page.locator("[data-kbc-set-spans] li").nth(1)).toContainText(KNOWN_FILE);

    // --- tour mode -----------------------------------------------------------
    await page.locator("[data-kbc-set-tour]").click();
    await expect(page).toHaveURL(/~tour/);
    await expect(page.locator("[data-kbc-tour-path]")).toContainText(CALLER_FILE);
    await expect(page.locator(".kbc-codeview")).toBeVisible({ timeout: 10_000 });

    await page.keyboard.press("ArrowRight");
    await expect(page.locator("[data-kbc-tour-path]")).toContainText(KNOWN_FILE);
    await expect(page.locator("[data-kbc-tour-counter]")).toHaveText("2/2");

    await page.locator("[data-kbc-tour-exit]").click();
    await expect(page).toHaveURL(/~sets\/set_[^/]+$/);

    // --- capture from the reader's "+ Set" menu -----------------------------
    await page.goto(`${BASE}/r/${REPO_NAME}/${KNOWN_FILE}`);
    await expect(page.locator(".kbc-codeview")).toBeVisible({ timeout: 10_000 });
    await page.locator("[data-kbc-addset-trigger]").click();
    await page.locator("[data-kbc-addset-item]", { hasText: "the ingest path" }).click();
    await expect(page.locator(".kbc-toast--ok")).toBeVisible();

    // The toast's own link re-opens the set — the capture appended a THIRD
    // span (a whole-file span for KNOWN_FILE — no selection was made).
    await page.locator(".kbc-toast--ok .kbc-toast__link").click();
    await expect(page.locator("[data-kbc-set-spans] li")).toHaveCount(3);

    // --- delete via confirm --------------------------------------------------
    await page.locator("[data-kbc-set-delete]").click();
    await page.locator(".confirm__go").click();
    await expect(page).toHaveURL(/~sets$/);
    await expect(page.locator("[data-kbc-sets-list]")).toHaveCount(0);
  });

  // From-session materialization needs a real transcript-backed session —
  // see this file's own header doc for why that isn't cheaply reachable in
  // the e2e fixture daemon.
  test.skip(
    "Save as reading set on SessionDiff (needs a real session — not e2e-able against this fixture daemon)",
    () => {},
  );
});
