import { execFileSync } from "node:child_process";
import { expect, test } from "@playwright/test";
import {
  FEATURE_BRANCH,
  FEATURE_FILE,
  FEATURE_X2_BRANCH,
  FEATURE_X2_FILE,
  STORY_FILE,
} from "./fixture-repo";
import { BASE, REPO_DIR, REPO_NAME } from "./helpers";

// V80 diff-highlight sweep — the four NON-review-diff surfaces that render
// a `DiffFile` / `UnifiedHunks` / painted `<pre>` of real source and
// previously fell back to plain text:
//
//   1. the review INTERDIFF's expandable file diff (InterdiffPanel),
//   2. the blame why-panel's "originating change" (OriginatingChange),
//   3. the in-thread suggestion editor's live preview (SuggestionEditor),
//   4. the apply-confirm working-tree slice (SuggestionDiff).
//
// `data-kbc-hl` is the painted-span contract `diff-syntax.spec.ts` already
// asserts on the commit page, and `highlight.spec.ts` on a review file —
// every test here fails if its surface regresses to unpainted text.
//
// Fixture discipline: no new commits, no new blobs. The interdiff's second
// patchset is produced by MOVING `feature-x-2` back onto `feature-x` (both
// are pre-indexed commits) and snapshotting again; the branch is restored
// in `afterAll` exactly as `reviews.spec.ts` restores `feature-x`. The
// suggestion tests check `feature-x` out so the apply preview's
// working-tree read has the file, and restore `main` in `afterAll`.

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

const SUGGESTION_THREAD = "e2e diff-hl suggestion";
const SUGGESTION_REPLACEMENT = "// e2e diff-hl replacement line";

/// `feature-x-2`'s tip before this spec moved it back onto `feature-x`
/// (see the interdiff test) — restored in `afterAll`.
let x2TipBefore: string | null = null;

test.describe("diff highlight — interdiff / blame-origin / suggestion surfaces", () => {
  test.afterAll(() => {
    tryGit(["checkout", "-f", "main"]);
    // Restore `feature-x-2`'s ORIGINAL tip — the stack spec pins that
    // branch, exactly as `reviews.spec.ts` restores `feature-x`.
    if (x2TipBefore) {
      tryGit(["branch", "-f", FEATURE_X2_BRANCH, x2TipBefore]);
    }
  });

  test("interdiff file diff paints its lines", async ({ page, request }) => {
    test.skip(!REPO_DIR, "KB_CODE_E2E_REPO_DIR not set — global-setup didn't run");
    tryGit(["checkout", "-f", "main"]);
    x2TipBefore = git(["rev-parse", FEATURE_X2_BRANCH]);

    // ps1 pins `feature-x-2` (a layer on top of `feature-x`).
    const createRes = await request.post(`${BASE}/api/reviews`, {
      data: {
        repo: REPO_NAME,
        head_ref: FEATURE_X2_BRANCH,
        base_ref: "main",
        title: "e2e diff hl interdiff",
      },
    });
    expect(createRes.ok(), `create review: ${createRes.status()} ${await createRes.text()}`).toBeTruthy();
    const created = (await createRes.json()) as { id: number };
    const reviewId = created.id;

    // ps2 pins `feature-x` — a pointer move, no new objects, so both tips
    // are blobs the daemon indexed at boot. The interdiff ps1→ps2 is the
    // `feature_x2.rs` file, whose spans exist on the `feature-x-2` side.
    git(["branch", "-f", FEATURE_X2_BRANCH, FEATURE_BRANCH]);
    const snapRes = await request.post(`${BASE}/api/reviews/${reviewId}/snapshot`);
    expect(snapRes.ok(), `snapshot: ${snapRes.status()} ${await snapRes.text()}`).toBeTruthy();
    const snap = (await snapRes.json()) as { ps_number: number };
    const ps2 = snap.ps_number;
    expect(ps2).toBeGreaterThanOrEqual(2);

    await page.goto(`${BASE}/r/${REPO_NAME}/~reviews/${reviewId}`);
    await expect(page.locator("[data-kbc-review-ps-strip]")).toBeVisible({ timeout: 15_000 });
    await page.locator("[data-kbc-review-compare]").click();
    await page.locator('[data-kbc-review-ps="1"]').click();
    await page.locator(`[data-kbc-review-ps="${ps2}"]`).click();
    await expect(page.locator("[data-kbc-review-interdiff]")).toBeVisible({ timeout: 15_000 });

    const row = page.locator(`[data-kbc-interdiff-file="${FEATURE_X2_FILE}"]`);
    await expect(row).toBeVisible({ timeout: 15_000 });
    await row.click();

    const body = page.locator(`.kbc-review__file-diff .kbc-diff`).last();
    await expect(body.locator(".kbc-diff__line").first()).toBeVisible({ timeout: 15_000 });
    // Direction-agnostic on purpose: whichever patchset order the panel
    // renders, ONE side is the `feature-x-2` blob and its lines are the
    // lines on screen — so "painted" is the claim, not a side.
    await expect(body.locator(".kbc-diff__line [data-kbc-hl]").first()).toBeVisible({
      timeout: 15_000,
    });
  });

  test("blame originating-change diff paints its lines", async ({ page }) => {
    test.skip(!REPO_DIR, "KB_CODE_E2E_REPO_DIR not set — global-setup didn't run");
    tryGit(["checkout", "-f", "main"]);

    await page.goto(`${BASE}/r/${REPO_NAME}`);
    await page.locator(".kbc-tree__row", { hasText: STORY_FILE }).click();
    // `story.rs` line 6 (`fn continue_story() -> i32 {`) was added by its
    // SECOND commit, so blame attributes it to a real commit whose parent
    // is real too — the one fixture line whose originating diff has both
    // sides. (`lib.rs` is initial-commit-only: its `sha^` does not
    // resolve, so the section renders the error arm instead.)
    await expect(page.locator(".kbc-codeview")).toContainText("continue_story", { timeout: 10_000 });

    const toggle = page.locator("[data-kbc-provenance-toggle]");
    await expect(toggle).toBeVisible();
    const blame = page.waitForResponse((res) => res.url().includes("/api/blame?"));
    const why = page.waitForResponse((res) => res.url().includes("/api/why?"));
    await toggle.click();
    await blame;
    await why;

    // Same coordinate-delegation the blame specs use: click the blame
    // gutter at the vertical centre of line 6's row.
    const lineSix = page.locator(".cm-lineNumbers .cm-gutterElement", { hasText: /^6$/ }).first();
    const lineBox = await lineSix.boundingBox();
    const gutterBox = await page.locator(".cm-gutter.kbc-blame-gutter").boundingBox();
    expect(lineBox).not.toBeNull();
    expect(gutterBox).not.toBeNull();
    await page.mouse.click(gutterBox!.x + gutterBox!.width / 2, lineBox!.y + lineBox!.height / 2);

    const origin = page.locator('[data-kbc-why-line="6"] [data-kbc-why-origin]');
    await expect(origin).toBeVisible({ timeout: 15_000 });
    const diff = origin.locator("[data-kbc-why-origin-diff]");
    await expect(diff).toBeVisible({ timeout: 15_000 });
    await expect(diff.locator(".kbc-diff__line--add [data-kbc-hl]").first()).toBeVisible({
      timeout: 15_000,
    });
  });

  test("suggestion editor preview + apply-confirm working tree paint", async ({ page, request }) => {
    test.skip(!REPO_DIR, "KB_CODE_E2E_REPO_DIR not set — global-setup didn't run");
    // Stay on `feature-x` so the apply preview's working-tree read finds
    // the file (same reason `suggestions.spec.ts` stays on its branch).
    tryGit(["checkout", "-f", "main"]);
    git(["checkout", "-q", FEATURE_BRANCH]);

    const createRes = await request.post(`${BASE}/api/reviews`, {
      data: {
        repo: REPO_NAME,
        head_ref: FEATURE_BRANCH,
        base_ref: "main",
        title: "e2e diff hl suggestions",
      },
    });
    expect(createRes.ok(), `create review: ${createRes.status()} ${await createRes.text()}`).toBeTruthy();
    const created = (await createRes.json()) as { id: number };
    const reviewId = created.id;

    await page.goto(`${BASE}/r/${REPO_NAME}/~reviews/${reviewId}`);
    await expect(page.locator("[data-kbc-review-files]")).toBeVisible({ timeout: 15_000 });
    await page.locator(`[data-kbc-review-file-row="${FEATURE_FILE}"]`).click();
    await expect(page).toHaveURL(new RegExp(`/diff/${FEATURE_FILE}(\\?|$)`));
    const diff = page.locator(`[data-kbc-rdiff-file="${FEATURE_FILE}"]`);
    await expect(diff).toBeVisible({ timeout: 15_000 });

    // Compose on the file's SECOND new-side line — `fn feature_x() -> i32 {`.
    // Deliberately not line 1: every painted surface here maps spans by
    // FILE line number, so a base of "snippet line 1" would still look
    // right at line 1. Line 2 makes a wrong base render plain.
    await diff.locator("[data-kbc-review-compose-new]").nth(1).click();
    const composer = page.locator("[data-kbc-review-composer]");
    await expect(composer).toBeVisible();
    await composer.locator("[data-kbc-review-composer-body]").fill(SUGGESTION_THREAD);
    await composer.locator("[data-kbc-review-composer-submit]").click();
    await page.locator("[data-kbc-rdiff-drafts-publish]").click();
    const thread = diff.locator("[data-kbc-review-thread]").filter({ hasText: SUGGESTION_THREAD });
    await expect(thread).toBeVisible({ timeout: 15_000 });
    const threadId = await thread.getAttribute("data-kbc-review-thread");
    expect(threadId).toBeTruthy();
    const threadRef = diff.locator(`[data-kbc-review-thread="${threadId}"]`);

    await threadRef.locator(`[data-kbc-suggestion-open="${threadId}"]`).click();
    const editor = threadRef.locator(`[data-kbc-suggestion-editor="${threadId}"]`);
    await expect(editor).toBeVisible();
    const cm = editor.locator("[data-kbc-suggestion-cm] .cm-content");
    await expect(cm).toBeVisible();
    await cm.click();
    await page.keyboard.press("Control+a");
    await page.keyboard.type(SUGGESTION_REPLACEMENT);
    // The ORIGINAL side of the preview is the anchored blob line, so it is
    // the remove row that must carry a token span.
    await expect(editor.locator("[data-kbc-suggestion-preview] .kbc-diff__line--remove")).toContainText(
      "fn feature_x",
    );
    await expect(
      editor.locator("[data-kbc-suggestion-preview] .kbc-diff__line--remove [data-kbc-hl]").first(),
    ).toBeVisible({ timeout: 15_000 });

    await editor.locator(`[data-kbc-suggestion-save="${threadId}"]`).click();
    await expect(threadRef.locator("[data-kbc-review-thread-suggestion]")).toBeVisible({
      timeout: 15_000,
    });
    await threadRef.locator(`[data-kbc-suggestion-apply="${threadId}"]`).click();
    await expect(page.locator(".confirm__go")).toBeVisible();
    const wt = page.locator("[data-kbc-suggestion-apply-preview] [data-kbc-suggestion-wt]");
    await expect(wt).toBeVisible({ timeout: 15_000 });
    await expect(wt).toContainText("fn feature_x");
    await expect(wt.locator("[data-kbc-hl]").first()).toBeVisible({ timeout: 15_000 });
    await page.locator(".confirm__cancel").click();
  });
});
