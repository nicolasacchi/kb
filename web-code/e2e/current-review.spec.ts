import { expect, test } from "@playwright/test";
import { KNOWN_FILE } from "./fixture-repo";
import { BASE, REPO_DIR, REPO_NAME } from "./helpers";

/// V80-M3 — "the current review travels with the reader." The browser-only
/// marker (`lib/currentReview.ts`) and its `?review=` reader-URL mirror
/// (`lib/codeUrl.ts` + `nav/location.ts`). No server-side fixture beyond a
/// plain local review (same `POST /api/reviews` pattern `review-room.spec.ts`
/// already established — `feature-x -> main`, never a throwaway branch).
///
/// Two flows, matching the brief:
///
///  A. Room → TopBar chip appears → navigate to a reader file → the URL
///     carries `?review=` and the rail's Review tab is selectable → clear
///     via the chip's `×` → the param is gone and the tab is gated again.
///  B. A COLD load of a reader URL carrying `?review=<id>` shows the chip
///     (the URL wins over — here, absent — session state).

test.describe("the current review travels with the reader (V80-M3)", () => {
  test("Room sets it, the reader carries it, the chip clears it", async ({ page, request }) => {
    test.skip(!REPO_DIR, "KB_CODE_E2E_REPO_DIR not set — global-setup didn't run");

    const createRes = await request.post(`${BASE}/api/reviews`, {
      data: {
        repo: REPO_NAME,
        head_ref: "feature-x",
        base_ref: "main",
        title: "e2e current review",
      },
    });
    expect(createRes.ok(), `create review: ${createRes.status()} ${await createRes.text()}`).toBeTruthy();
    const created = (await createRes.json()) as { id: number };
    const reviewId = created.id;

    // --- A1: entering the Room auto-sets the marker; the TopBar chip shows ---
    await page.goto(`${BASE}/r/${REPO_NAME}/~reviews/${reviewId}`);
    await expect(page.locator("[data-kbc-review-title]")).toContainText("e2e current review");
    const chip = page.locator('[data-kbc-current-review-chip="bar"]');
    await expect(chip).toBeVisible({ timeout: 10_000 });
    await expect(chip.locator("[data-kbc-current-review-room-link]")).toContainText(
      "e2e current review",
    );

    // The Room's own explicit re-affirmation button — already active on
    // entry (no click needed), so it reads "Working this review ✓" and is
    // disabled rather than idle "Work this review".
    const workBtn = page.locator("[data-kbc-review-work-this]");
    await expect(workBtn).toHaveText("Working this review ✓");
    await expect(workBtn).toBeDisabled();

    // --- A2: the review tab is NOT yet offered on a reader page we haven't
    // visited (sanity — proves the next assertion is the effect firing, not
    // an always-on tab) ------------------------------------------------------
    await page.goto(`${BASE}/r/${REPO_NAME}/${KNOWN_FILE}`);
    // The session marker is set (from the Room visit) — the reader's own
    // sync effect re-appends `?review=` via one replace even though this
    // landing URL didn't carry it.
    await expect(page).toHaveURL(new RegExp(`review=${reviewId}(&|$)`), { timeout: 10_000 });
    const reviewTab = page.locator('[data-kbc-itab="review"]');
    await expect(reviewTab).toBeVisible({ timeout: 10_000 });
    await reviewTab.click();
    const reviewPanel = page.locator("[data-kbc-current-review-panel]");
    await expect(reviewPanel).toBeVisible();
    await expect(reviewPanel).toContainText("Threads for this file arrive with M2.");
    // Scoped to the panel — the SAME attribute also marks the TopBar chip's
    // own Room link (both are honestly "the room link", just two homes).
    await expect(reviewPanel.locator("[data-kbc-current-review-room-link]")).toBeVisible();

    // Switch off the Review tab before clearing, so "gated again" is
    // observable as the tab disappearing from the strip (rather than
    // staying selected and merely showing its empty body — InspectorRail's
    // own documented carve-out for a PERSISTED selection).
    await page.locator('[data-kbc-itab="understand"]').click();

    // --- A3: clearing via the chip's × strips the URL param and re-gates
    // the tab --------------------------------------------------------------
    await chip.locator("[data-kbc-current-review-clear]").click();
    await expect(page).not.toHaveURL(/review=/, { timeout: 10_000 });
    await expect(page.locator('[data-kbc-current-review-chip="bar"]')).toHaveCount(0);
    await expect(page.locator('[data-kbc-itab="review"]')).toHaveCount(0);

    // --- B: a COLD load of a reader URL carrying `?review=` shows the chip
    // even with no prior session state (a fresh context has none) ----------
    const fresh = await page.context().browser()!.newContext();
    const freshPage = await fresh.newPage();
    await freshPage.goto(`${BASE}/r/${REPO_NAME}/${KNOWN_FILE}?review=${reviewId}`);
    await expect(freshPage.locator('[data-kbc-current-review-chip="bar"]')).toBeVisible({
      timeout: 10_000,
    });
    await expect(freshPage.locator('[data-kbc-itab="review"]')).toBeVisible();
    await fresh.close();
  });
});
