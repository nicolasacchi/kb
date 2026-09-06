import { execFileSync } from "node:child_process";
import { readFileSync, writeFileSync } from "node:fs";
import { join } from "node:path";
import { expect, test, type Page } from "@playwright/test";
import { FEATURE_BRANCH, FEATURE_FILE } from "./fixture-repo";
import { BASE, REPO_DIR, REPO_NAME } from "./helpers";

// U1 (PRR-U1) — the ~reviews session table is demoted into a collapsed
// `<details>` (`kbc-inbox-browse`, the same `BrowseAllBranches` precedent
// `branches-landing.spec.ts` handles this way), open by default only when
// `list.length <= 5`. The shared e2e daemon accumulates many review
// sessions across earlier specs in the same worker run, so by the time this
// file's own list-page assertions run the fold is closed and its rows are
// native-hidden — open it first.
async function openReviewsBrowse(page: Page) {
  const browse = page.locator("[data-kbc-browse]");
  await expect(browse).toBeVisible({ timeout: 10_000 });
  // The `<details open={list.length <= 5}>` prop is recomputed on every
  // render: while `useReviews` is still loading, `list` is `[]` (<=5, so
  // the fold starts OPEN), then snaps CLOSED once the real (larger) count
  // lands — checking hidden-state before that query settles races the
  // transition and can catch it mid-flight. Wait the loading hint out
  // first so the `open` prop has reached its final, stable value.
  await expect(browse.getByText("Loading reviews…")).toHaveCount(0, { timeout: 10_000 });
  const list = page.locator("[data-kbc-reviews-list]");
  if (await list.isHidden()) {
    await browse.locator("[data-kbc-browse-toggle]").click();
  }
  await expect(list).toBeVisible({ timeout: 10_000 });
}

/// V4.C4 — review comment threads + verdict UI. Creates a disposable
/// branch off feature-x (adds a README.md edit so split mode has an
/// old-side line). Does NOT run Playwright in this builder — the
/// orchestrator gates that; this file is typechecked with the e2e tsc.

const THREADS_BRANCH = "e2e-review-threads";
const README = "README.md";

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

test.describe("review threads + verdict (V4.C4)", () => {
  test.afterAll(() => {
    tryGit(["checkout", "main"]);
    tryGit(["branch", "-D", THREADS_BRANCH]);
  });

  test("compose, reply, resolve, old-side, orphan, deep-link, verdict", async ({ page, request }) => {
    test.skip(!REPO_DIR, "KB_CODE_E2E_REPO_DIR not set — global-setup didn't run");

    tryGit(["checkout", "main"]);
    tryGit(["branch", "-D", THREADS_BRANCH]);
    git(["checkout", "-q", "-b", THREADS_BRANCH, FEATURE_BRANCH]);
    const readme = readFileSync(join(REPO_DIR, README), "utf-8");
    writeFileSync(join(REPO_DIR, README), readme.replace("# fixture", "# fixture threads"));
    git(["add", README]);
    git(["commit", "-q", "-m", "e2e threads: edit README for old-side"]);
    git(["checkout", "-q", "main"]);

    const createRes = await request.post(`${BASE}/api/reviews`, {
      data: {
        repo: REPO_NAME,
        head_ref: THREADS_BRANCH,
        base_ref: "main",
        title: "e2e review threads",
      },
    });
    expect(createRes.ok(), `create review: ${createRes.status()} ${await createRes.text()}`).toBeTruthy();
    const created = (await createRes.json()) as { id: number };
    const reviewId = created.id;

    await page.goto(`${BASE}/r/${REPO_NAME}/~reviews/${reviewId}`);
    await expect(page.locator("[data-kbc-review-title]")).toContainText("e2e review threads");
    await expect(page.locator("[data-kbc-review-files]")).toBeVisible({ timeout: 10_000 });
    await expect(page.locator("[data-kbc-review-threads]")).toBeVisible();
    await expect(page.locator("[data-kbc-review-verdict-bar]")).toBeVisible();

    // --- compose on a new-side line in the cockpit ------------------------
    await page.locator(`[data-kbc-review-file-row="${FEATURE_FILE}"]`).click();
    const featDiff = page.locator(`[data-kbc-review-file-diff="${FEATURE_FILE}"]`);
    await expect(featDiff).toBeVisible({ timeout: 10_000 });
    await featDiff.locator("[data-kbc-review-compose-new]").first().click();
    const composer = page.locator("[data-kbc-review-composer]");
    await expect(composer).toBeVisible();
    await composer.locator("[data-kbc-review-composer-body]").fill("thread from cockpit");
    await composer.locator("[data-kbc-review-composer-submit]").click();
    const thread = featDiff.locator("[data-kbc-review-thread]").first();
    await expect(thread).toBeVisible({ timeout: 10_000 });
    await expect(thread).toContainText("thread from cockpit");
    const threadId = await thread.getAttribute("data-kbc-review-thread");
    expect(threadId).toBeTruthy();

    // --- reply ------------------------------------------------------------
    await thread.locator(`[data-kbc-review-thread-reply-body="${threadId}"]`).fill("a reply");
    await thread.locator(`[data-kbc-review-thread-reply-submit="${threadId}"]`).click();
    await expect(thread.locator("[data-kbc-review-thread-reply]")).toContainText("a reply", {
      timeout: 10_000,
    });

    // --- resolve → collapses ----------------------------------------------
    await thread.locator(`[data-kbc-review-thread-resolve="${threadId}"]`).click();
    await expect(thread).toHaveAttribute("data-kbc-review-thread-resolved", "true", {
      timeout: 10_000,
    });
    await expect(thread.locator("[data-kbc-review-thread-expand]")).toContainText("resolved");

    // --- a second, deliberately UNRESOLVED thread: the orphan flow below
    // must target an OPEN thread (the default comments view excludes
    // resolved threads — a resolved thread is already triaged, ratified).
    await featDiff.locator("[data-kbc-review-compose-new]").first().click();
    const baitComposer = page.locator("[data-kbc-review-composer]");
    await expect(baitComposer).toBeVisible();
    await baitComposer.locator("[data-kbc-review-composer-body]").fill("orphan bait");
    await baitComposer.locator("[data-kbc-review-composer-submit]").click();
    const baitThread = featDiff
      .locator("[data-kbc-review-thread]")
      .filter({ hasText: "orphan bait" });
    await expect(baitThread).toBeVisible({ timeout: 10_000 });
    const baitThreadId = await baitThread.getAttribute("data-kbc-review-thread");
    expect(baitThreadId).toBeTruthy();

    // --- old-side compose in split mode (README.md has a remove line) -----
    await page.locator(`[data-kbc-review-file-row="${README}"]`).click();
    const readmeDiff = page.locator(`[data-kbc-review-file-diff="${README}"]`);
    await expect(readmeDiff).toBeVisible({ timeout: 10_000 });
    await readmeDiff.locator('[data-kbc-diff-mode-toggle="split"]').click();
    await expect(readmeDiff.locator("[data-kbc-sdiff]")).toBeVisible();
    await readmeDiff.locator("[data-kbc-review-compose-old]").first().click();
    const oldComposer = readmeDiff.locator("[data-kbc-review-composer]");
    await expect(oldComposer).toBeVisible();
    await oldComposer.locator("[data-kbc-review-composer-body]").fill("old-side note");
    await oldComposer.locator("[data-kbc-review-composer-submit]").click();
    await expect(readmeDiff.locator("[data-kbc-review-thread]")).toContainText("old-side note", {
      timeout: 10_000,
    });

    // --- orphan: amend FEATURE_FILE + snapshot ps2 ------------------------
    git(["checkout", "-q", THREADS_BRANCH]);
    writeFileSync(
      join(REPO_DIR, FEATURE_FILE),
      ["// added on feature-x (amended)", "fn feature_x() -> i32 {", "    1", "}", ""].join("\n"),
    );
    git(["add", FEATURE_FILE]);
    git(["commit", "-q", "-m", "e2e threads: amend feature file"]);
    git(["checkout", "-q", "main"]);

    const snapRes = await request.post(`${BASE}/api/reviews/${reviewId}/snapshot`);
    expect(snapRes.ok(), `snapshot: ${snapRes.status()} ${await snapRes.text()}`).toBeTruthy();
    const snap = (await snapRes.json()) as { ps_number: number };
    expect(snap.ps_number).toBeGreaterThanOrEqual(2);

    await page.reload();
    // V70-H1 — this review carries more comment/thread state (compose,
    // reply, resolve, a second thread, an old-side note) than the
    // near-identical reload-after-snapshot in `reviews.spec.ts` (which
    // passes at the same default 10s under this same suite), so the
    // orphan-detection recompute this reload triggers is genuinely
    // heavier, not just unluckier — a real (not self-clearing, unlike
    // `.kbc-rdiff__flash`) render-completion wait, so a longer budget is a
    // correct fix, not a mask.
    await expect(page.locator("[data-kbc-review-files]")).toBeVisible({ timeout: 20_000 });
    await page.locator(`[data-kbc-review-file-row="${FEATURE_FILE}"]`).click();
    const featDiff2 = page.locator(`[data-kbc-review-file-diff="${FEATURE_FILE}"]`);
    await expect(featDiff2.locator("[data-kbc-review-orphans]")).toBeVisible({ timeout: 20_000 });
    await expect(featDiff2.locator("[data-kbc-review-orphan-was]")).toContainText(/was ps\d+:L\d+/);

    // --- ThreadsCard deep-link --------------------------------------------
    const cardRow = page.locator(`[data-kbc-review-threads-row="${baitThreadId}"]`);
    await page.locator('[data-kbc-review-threads-filter="all"]').click();
    await expect(cardRow).toBeVisible({ timeout: 10_000 });
    await cardRow.click();
    await expect(page).toHaveURL(new RegExp(`~reviews/${reviewId}/diff/`));
    await expect(page.locator("[data-kbc-rdiff]")).toBeVisible({ timeout: 10_000 });
    const deepThread = page.locator(`[data-kbc-review-thread="${baitThreadId}"]`);
    await expect(deepThread).toBeVisible({ timeout: 10_000 });

    // --- verdict approve → chip in header + list row ----------------------
    await page.locator("[data-kbc-rdiff-back]").click();
    await expect(page.locator("[data-kbc-review-verdict-bar]")).toBeVisible({ timeout: 10_000 });
    await page.locator('[data-kbc-review-verdict="approve"]').click();
    await expect(page.locator('[data-kbc-review-verdict-chip="approve"]')).toBeVisible({
      timeout: 10_000,
    });
    // no-op re-PUT of the same state keeps the chip
    await page.locator('[data-kbc-review-verdict="approve"]').click();
    await expect(page.locator('[data-kbc-review-verdict-chip="approve"]')).toBeVisible();

    await page.goto(`${BASE}/r/${REPO_NAME}/~reviews`);
    await openReviewsBrowse(page);
    const listRow = page.locator(`[data-kbc-reviews-row="${reviewId}"]`);
    await expect(listRow).toBeVisible({ timeout: 10_000 });
    await expect(listRow.locator('[data-kbc-reviews-verdict="approve"]')).toBeVisible();

    // --- another snapshot → stale hint ------------------------------------
    git(["checkout", "-q", THREADS_BRANCH]);
    writeFileSync(join(REPO_DIR, FEATURE_FILE), [
      "// added on feature-x (amended again)",
      "fn feature_x() -> i32 {",
      "    2",
      "}",
      "",
    ].join("\n"));
    git(["add", FEATURE_FILE]);
    git(["commit", "-q", "-m", "e2e threads: third patchset"]);
    git(["checkout", "-q", "main"]);

    const snap2 = await request.post(`${BASE}/api/reviews/${reviewId}/snapshot`);
    expect(snap2.ok(), `snapshot2: ${snap2.status()} ${await snap2.text()}`).toBeTruthy();

    await page.goto(`${BASE}/r/${REPO_NAME}/~reviews/${reviewId}`);
    await expect(page.locator("[data-kbc-review-verdict-stale]")).toBeVisible({ timeout: 10_000 });
    await expect(page.locator("[data-kbc-review-verdict-stale-hint]")).toContainText("approved at ps");
    await page.goto(`${BASE}/r/${REPO_NAME}/~reviews`);
    await openReviewsBrowse(page);
    await expect(
      page.locator(`[data-kbc-reviews-row="${reviewId}"] [data-kbc-reviews-verdict-stale]`),
    ).toBeVisible({ timeout: 10_000 });
  });
});
