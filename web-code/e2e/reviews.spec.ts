import { execFileSync } from "node:child_process";
import { writeFileSync } from "node:fs";
import { join } from "node:path";
import { expect, test } from "@playwright/test";
import { FEATURE_BRANCH, FEATURE_FILE } from "./fixture-repo";
import { BASE, REPO_DIR, REPO_NAME } from "./helpers";

/// V3.R2 — review cockpit end to end against the loopback test daemon.
/// Creates a review via the API (not the SPA dialog), opens ~reviews, opens
/// detail, toggles viewed, then snapshots a second patchset and exercises
/// interdiff mode. Does NOT mutate main's tip (feature-x only).

function git(args: string[]): string {
  return execFileSync("git", ["-C", REPO_DIR, ...args], { encoding: "utf-8" }).trim();
}

function tryGit(args: string[]): void {
  try {
    execFileSync("git", ["-C", REPO_DIR, ...args], { stdio: "ignore" });
  } catch {
    // best-effort cleanup
  }
}

let featureTipBefore: string | null = null;

test.describe("reviews cockpit (V3.R2)", () => {
  test.afterAll(() => {
    // Restore feature-x's ORIGINAL tip: this spec adds a second commit on
    // feature-x for the interdiff patchset, and time.spec (which runs
    // later) pins feature-x at "ahead 1" with the fixture subject —
    // leaving our commit behind broke it in the full-suite order
    // (2026-08-01). The review's refs/kbc/review/* patchset refs keep the
    // e2e commit object alive independently of the branch tip.
    tryGit(["checkout", "main"]);
    if (featureTipBefore) {
      tryGit(["branch", "-f", FEATURE_BRANCH, featureTipBefore]);
    }
  });

  test("list, detail files, viewed, interdiff", async ({ page, request }) => {
    test.skip(!REPO_DIR, "KB_CODE_E2E_REPO_DIR not set — global-setup didn't run");

    // --- create review via API against feature-x → main -------------------
    const createRes = await request.post(`${BASE}/api/reviews`, {
      data: {
        repo: REPO_NAME,
        head_ref: FEATURE_BRANCH,
        base_ref: "main",
        title: "e2e review cockpit",
      },
    });
    expect(createRes.ok(), `create review: ${createRes.status()} ${await createRes.text()}`).toBeTruthy();
    const created = (await createRes.json()) as { id: number; latest_ps: number };
    expect(created.id).toBeGreaterThan(0);
    const reviewId = created.id;

    // --- list page shows it -----------------------------------------------
    await page.goto(`${BASE}/r/${REPO_NAME}/~reviews`);
    // U1 (PRR-U1) — the session table is demoted into a collapsed
    // `<details>` (`kbc-inbox-browse`, the same `BrowseAllBranches`
    // precedent `branches-landing.spec.ts` already handles this way), open
    // by default only when `list.length <= 5`; the shared e2e daemon
    // accumulates many review sessions across earlier specs in the same
    // worker run, so by the time this test runs the fold is closed and its
    // list is native-hidden.
    const browse = page.locator("[data-kbc-browse]");
    await expect(browse).toBeVisible({ timeout: 10_000 });
    // The `<details open={list.length <= 5}>` prop is recomputed on every
    // render: while `useReviews` is still loading, `list` is `[]` (<=5, so
    // the fold starts OPEN), then snaps CLOSED once the real (larger)
    // count lands — checking hidden-state before that query settles races
    // the transition. Wait the loading hint out first.
    await expect(browse.getByText("Loading reviews…")).toHaveCount(0, { timeout: 10_000 });
    const list = page.locator("[data-kbc-reviews-list]");
    if (await list.isHidden()) {
      await browse.locator("[data-kbc-browse-toggle]").click();
    }
    await expect(list).toBeVisible({ timeout: 10_000 });
    const row = page.locator(`[data-kbc-reviews-row="${reviewId}"]`);
    await expect(row).toBeVisible();
    await expect(row.locator(`[data-kbc-reviews-row-link="${reviewId}"]`)).toContainText(
      "e2e review cockpit",
    );

    // --- detail: files table ----------------------------------------------
    await row.locator(`[data-kbc-reviews-row-link="${reviewId}"]`).click();
    await expect(page).toHaveURL(new RegExp(`~reviews/${reviewId}`));
    await expect(page.locator("[data-kbc-review-title]")).toContainText("e2e review cockpit");
    await expect(page.locator("[data-kbc-review-files]")).toBeVisible({ timeout: 10_000 });
    // feature-x adds FEATURE_FILE
    await expect(page.locator(`[data-kbc-review-file="${FEATURE_FILE}"]`)).toBeVisible();

    // --- toggle viewed persists + progress updates ------------------------
    const progress = page.locator("[data-kbc-review-progress]");
    await expect(progress).toBeVisible();
    const before = await progress.getAttribute("data-kbc-review-progress");
    const checkbox = page.locator(`[data-kbc-review-viewed="${FEATURE_FILE}"]`);
    // click(), not check(): the box is a CONTROLLED input whose checked
    // state is server-derived (#23 — mutation → invalidation → refetch),
    // so it does not flip synchronously; check()'s instant-state assert
    // fails by design. The progress-attribute wait below is the real
    // assertion that the round-trip landed.
    await checkbox.click();
    // Progress attribute updates after mutation + query invalidation.
    await expect(progress).not.toHaveAttribute("data-kbc-review-progress", before ?? "", {
      timeout: 10_000,
    });
    const after = await progress.getAttribute("data-kbc-review-progress");
    expect(after).toMatch(/^\d+\/\d+$/);
    const [viewedAfter] = (after ?? "0/0").split("/").map(Number);
    expect(viewedAfter).toBeGreaterThan(0);

    // --- second patchset via API, then interdiff in SPA -------------------
    // Advance feature-x tip with an additive commit (do not touch main).
    // Tip restored in afterAll — see the restore note there.
    featureTipBefore = git(["rev-parse", FEATURE_BRANCH]);
    git(["checkout", FEATURE_BRANCH]);
    writeFileSync(
      join(REPO_DIR, FEATURE_FILE),
      "// e2e second patchset body\npub fn feature_x_e2e() {}\n",
    );
    git(["add", FEATURE_FILE]);
    git(["commit", "-q", "-m", "e2e review second patchset"]);
    git(["checkout", "main"]);

    const snapRes = await request.post(`${BASE}/api/reviews/${reviewId}/snapshot`);
    expect(snapRes.ok(), `snapshot: ${snapRes.status()} ${await snapRes.text()}`).toBeTruthy();
    const snap = (await snapRes.json()) as { ps_number: number };
    expect(snap.ps_number).toBeGreaterThanOrEqual(2);

    // Reload detail so patchset strip includes ps2.
    await page.reload();
    await expect(page.locator("[data-kbc-review-ps-strip]")).toBeVisible({ timeout: 10_000 });
    await expect(page.locator('[data-kbc-review-ps="1"]')).toBeVisible();
    await expect(page.locator(`[data-kbc-review-ps="${snap.ps_number}"]`)).toBeVisible();

    await page.locator("[data-kbc-review-compare]").click();
    await page.locator('[data-kbc-review-ps="1"]').click();
    await page.locator(`[data-kbc-review-ps="${snap.ps_number}"]`).click();
    await expect(page.locator("[data-kbc-review-interdiff]")).toBeVisible({ timeout: 10_000 });
    // Range-diff table (shared component) or files list should appear.
    await expect(
      page.locator("[data-kbc-review-interdiff-files], [data-kbc-rangediff-table]").first(),
    ).toBeVisible();
  });
});
