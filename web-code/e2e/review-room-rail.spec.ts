import { expect, test } from "@playwright/test";
import { FEATURE_BRANCH, FEATURE_FILE } from "./fixture-repo";
import { BASE, REPO_DIR, REPO_NAME } from "./helpers";

/// V76-R2a — the Review Room's resizable findings rail + Report hero, in one
/// flow: create a review, import two findings (a blocker and a verified-ok),
/// PUT a report WITHOUT agent metadata (so the hero's agent block must be
/// ABSENT — never an "UNSET" box), then drive the SPA:
///
///  - the hero renders the three live counts as chips + the viewed meter
///  - no "UNSET" string appears anywhere on the page
///  - the rail separator drags wider, the width persists across a reload
///  - `Space i` collapses the rail to its stripe and re-expands it
///  - `Space I` resets the width to the default
///
/// Same no-git-choreography posture as `review-room.spec.ts` — this test
/// only ever POSTs against the daemon's local review/findings/report tables.

const BLOCKER_SLUG = "f-e2e-r76-blocker";
const OK_SLUG = "f-e2e-r76-ok";

test.describe("review room V76 — resizable findings rail + report hero", () => {
  test("rail resizes and persists; the hero renders counts; never an UNSET box", async ({
    page,
    request,
  }) => {
    test.skip(!REPO_DIR, "KB_CODE_E2E_REPO_DIR not set — global-setup didn't run");

    // --- create the review -------------------------------------------------
    const createRes = await request.post(`${BASE}/api/reviews`, {
      data: {
        repo: REPO_NAME,
        head_ref: FEATURE_BRANCH,
        base_ref: "main",
        title: "e2e v76 room",
      },
    });
    expect(createRes.ok(), `create review: ${createRes.status()} ${await createRes.text()}`).toBeTruthy();
    const reviewId = ((await createRes.json()) as { id: number }).id;

    // --- import two findings (loopback) ------------------------------------
    const importRes = await request.post(`${BASE}/api/reviews/${reviewId}/findings/import`, {
      data: {
        schema: "kbc-findings/1",
        findings: [
          {
            slug: BLOCKER_SLUG,
            severity: "blocker",
            category: "Correctness",
            location: { path: FEATURE_FILE, kind: "single", lines: [2] },
            title: "e2e v76 blocker finding",
            rationale: "e2e fixture blocker rationale.",
          },
          {
            slug: OK_SLUG,
            severity: "ok",
            category: "Tests",
            location: { path: FEATURE_FILE, kind: "whole_file" },
            title: "e2e v76 verified finding",
            rationale: "e2e fixture verified rationale.",
          },
        ],
      },
    });
    expect(importRes.ok(), `import findings: ${importRes.status()} ${await importRes.text()}`).toBeTruthy();

    // --- a report WITHOUT authored_by/session_id: the agent block must hide -
    const reportRes = await request.put(`${BASE}/api/reviews/${reviewId}/report`, {
      data: {
        schema: "kbc-report/1",
        summary: "## Summary\n\ne2e v76 report summary.",
        verdict: "blocker",
        verdict_headline: "e2e v76 needs a fix",
        risk_score: 7,
      },
    });
    expect(reportRes.ok(), `put report: ${reportRes.status()} ${await reportRes.text()}`).toBeTruthy();

    // --- the hero renders counts from the wire ------------------------------
    await page.goto(`${BASE}/r/${REPO_NAME}/~reviews/${reviewId}`);
    const hero = page.locator("[data-kbc-room-hero]");
    await expect(hero).toBeVisible({ timeout: 10_000 });
    await expect(hero.locator('[data-kbc-room-hero-count="blocker"]')).toContainText("1 blocker");
    await expect(hero.locator('[data-kbc-room-hero-count="concern"]')).toContainText("0 concerns");
    await expect(hero.locator('[data-kbc-room-hero-count="ok"]')).toContainText("1 verified");
    await expect(hero.locator("[data-kbc-room-hero-viewed]")).toContainText("files viewed");
    // No agent metadata anywhere on this review — the block is ABSENT…
    await expect(hero.locator("[data-kbc-room-hero-agent]")).toHaveCount(0);
    // …and the string "UNSET" appears nowhere on the page.
    await expect(page.getByText(/unset/i)).toHaveCount(0);

    // --- the rail resizes by drag, and the width persists -------------------
    const sep = page.locator("[data-kbc-room-sep]");
    await expect(sep).toBeVisible();
    const rail = page.locator('[data-region="review-side"]');
    await expect(rail).toBeVisible();
    const before = (await rail.boundingBox())!.width;
    const sepBox = (await sep.boundingBox())!;
    await page.mouse.move(sepBox.x + sepBox.width / 2, sepBox.y + 200);
    await page.mouse.down();
    await page.mouse.move(sepBox.x - 160, sepBox.y + 200, { steps: 8 });
    await page.mouse.up();
    const after = (await rail.boundingBox())!.width;
    expect(after, "dragging the separator left grows the rail").toBeGreaterThan(before + 40);
    // The reducer persisted the new width…
    const stored = await page.evaluate(() => localStorage.getItem("kbc:review-rail"));
    expect(stored).toBeTruthy();
    const storedWidth = (JSON.parse(stored!) as { width: number }).width;
    expect(storedWidth).toBeGreaterThan(25); // dragged wider than the 25% default
    // …and a reload restores it (within a few px of rounding).
    await page.reload();
    const railAgain = page.locator('[data-region="review-side"]');
    await expect(railAgain).toBeVisible({ timeout: 10_000 });
    const persisted = (await railAgain.boundingBox())!.width;
    expect(Math.abs(persisted - after)).toBeLessThan(24);

    // --- Space i collapses the rail to its stripe, and re-expands it --------
    await page.keyboard.press("Space");
    await page.keyboard.press("i");
    await expect(page.locator("[data-kbc-room-rail-expand]")).toBeVisible({ timeout: 10_000 });
    await page.keyboard.press("Space");
    await page.keyboard.press("i");
    await expect(page.locator("[data-kbc-room-rail-expand]")).toHaveCount(0);
    await expect(page.locator('[data-region="review-side"]')).toBeVisible();

    // --- Space I resets the width to the default ----------------------------
    await page.keyboard.press("Space");
    await page.keyboard.press("I");
    const reset = await page.evaluate(() => localStorage.getItem("kbc:review-rail"));
    expect((JSON.parse(reset!) as { width: number }).width).toBe(25);

    // --- a hero count chip filters the rail ---------------------------------
    await page.locator('[data-kbc-room-hero-count="blocker"]').click();
    await expect(
      page.locator('[data-kbc-finding-filter-severity="blocker"]'),
    ).toHaveAttribute("aria-pressed", "true");
    await expect(page.locator(`[data-kbc-finding-row="${BLOCKER_SLUG}"]`)).toBeVisible();
    await expect(page.locator(`[data-kbc-finding-row="${OK_SLUG}"]`)).toHaveCount(0);
  });
});
