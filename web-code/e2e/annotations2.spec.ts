import { expect, test } from "@playwright/test";
import { FEATURE_BRANCH, FEATURE_FILE, KNOWN_FILE, KNOWN_SYMBOL } from "./fixture-repo";
import { BASE, REPO_NAME } from "./helpers";

/// Phase D-SPA ("kb-code v2 — The Operable Reader") end to end: annotations
/// v2 — a visual-mode range composer, threads (reply + resolve), the
/// intent filter row, and diff-anchored comments on the Commit page.
/// Shares the SAME running daemon + fixture repo every other e2e spec in
/// this suite does (`playwright.config.ts`'s `workers: 1`), so every test
/// here uses its own distinctive body text to stay independent of
/// whatever annotations earlier tests (in this file or another) already
/// left behind — none of it is ever cleaned up, by design (mirrors the
/// rest of this suite's "additive fixture, never assert an exact global
/// count" discipline, e.g. `time.spec.ts`'s own `feature-x` branch).

async function openFixtureFile(page: import("@playwright/test").Page) {
  await page.goto(`${BASE}/r/${REPO_NAME}`);
  await page.locator(".kbc-tree__row", { hasText: KNOWN_FILE }).click();
  await expect(page.locator(".kbc-codeview")).toContainText(KNOWN_SYMBOL, { timeout: 10_000 });
  // Deterministic focus + cursor position at line 1 — same idiom
  // `reader-vim.spec.ts` uses.
  await page.locator(".kbc-codeview .cm-line").first().click();
}

interface RefsBody {
  refs: Array<{ name: string; kind: string; target_sha: string }>;
}

/// `feature-x`'s own HEAD sha, discovered via the live API (mirrors
/// `time.spec.ts`'s own `initialCommitSha` helper) — its one commit is NOT a
/// root commit (main's initial commit is its parent), so its Commit page
/// actually allows the per-file diff to expand (a root commit's
/// `FileChangeRow` disables expansion entirely — see `routes/Commit.tsx`'s
/// `isRoot` branch), which is required for the diff-comment affordance
/// under test.
async function featureBranchHeadSha(): Promise<string> {
  const res = await fetch(`${BASE}/api/refs?repo=${REPO_NAME}`);
  const body = (await res.json()) as RefsBody;
  const ref = body.refs.find((r) => r.kind === "branch" && r.name === FEATURE_BRANCH);
  if (!ref) throw new Error(`fixture bug: ${FEATURE_BRANCH} branch not found in GET /api/refs`);
  return ref.target_sha;
}

test.describe("annotations v2 — range composer", () => {
  test("visual-select two lines, `a` shows a range composer; saving lists an L1–2 badge", async ({ page }) => {
    await openFixtureFile(page);

    // v + j — visual mode over lines 1-2 (same gesture `reader-vim.spec.ts`
    // proves lands `?line=1-2`); `a` then opens the composer with THAT
    // range, not just the cursor's line.
    await page.keyboard.press("v");
    await page.keyboard.press("j");
    await page.keyboard.press("a");

    const rangeBadge = page.locator("[data-kbc-annot-range-badge]");
    await expect(rangeBadge).toHaveText("L1–2");
    // No editable line-number input while a range is active.
    await expect(page.locator("[data-kbc-annot-line-input]")).toHaveCount(0);
    // A range and "attach to symbol" are mutually exclusive.
    await expect(page.locator("[data-kbc-annot-symbol-toggle]")).toBeDisabled();

    const body = "D5 range comment for e2e";
    await page.locator("[data-kbc-annot-body-input]").fill(body);
    await page.locator("[data-kbc-annot-save]").click();

    const thread = page.locator(".kbc-annotations__thread", { hasText: body });
    await expect(thread).toBeVisible();
    await expect(thread.locator("[data-kbc-annot-goto]")).toHaveText("L1–2");

    // Collapse the visual-mode selection left over from `v`+`j` above by
    // clicking elsewhere in the buffer (a plain click always collapses to
    // a cursor, regardless of DOM focus at the time) — so the assertion
    // below is actually exercising the goto click, not a stale carry-over
    // selection.
    await page.locator(".kbc-codeview .cm-line").last().click();
    await expect(page.locator(".kbc-codeview .cm-selectionBackground")).toHaveCount(0);

    // Clicking the range badge RE-selects the WHOLE span in the buffer
    // (the same `.cm-selectionBackground` assertion `reader-vim.spec.ts`
    // uses for a hard-navigated `?line=2-4` deep link).
    await thread.locator("[data-kbc-annot-goto]").click();
    await expect(page.locator(".kbc-codeview .cm-selectionBackground").first()).toBeVisible({ timeout: 5_000 });
  });
});

test.describe("annotations v2 — threads", () => {
  test("reply + resolve a thread", async ({ page }) => {
    await openFixtureFile(page);

    // A plain single-line annotation: normal-mode `a` (no visual selection
    // active), so the composer shows the editable line input, not a range
    // badge.
    await page.keyboard.press("a");
    await expect(page.locator("[data-kbc-annot-line-input]")).toBeVisible();
    await expect(page.locator("[data-kbc-annot-range-badge]")).toHaveCount(0);

    const parentBody = "D5 parent comment for e2e";
    await page.locator("[data-kbc-annot-body-input]").fill(parentBody);
    await page.locator("[data-kbc-annot-save]").click();

    const thread = page.locator(".kbc-annotations__thread", { hasText: parentBody });
    await expect(thread).toBeVisible();

    // Reply.
    await thread.locator("[data-kbc-annot-reply-toggle]").click();
    const replyBody = "D5 reply for e2e";
    await thread.locator("[data-kbc-annot-reply-input]").fill(replyBody);
    await thread.locator("[data-kbc-annot-reply-save]").click();
    await expect(thread.locator(".kbc-annotations__reply", { hasText: replyBody })).toBeVisible();

    // Resolve — resolves the whole thread visually, not just the parent
    // text.
    await expect(thread.locator("[data-kbc-annot-resolve]")).toHaveText("Resolve");
    await thread.locator("[data-kbc-annot-resolve]").click();
    await expect(thread).toHaveClass(/is-resolved/);
    await expect(thread.locator("[data-kbc-annot-resolve]")).toHaveText("Reopen");
  });
});

test.describe("annotations v2 — intent filter", () => {
  test("the flagged filter shows only flag-for-agent items", async ({ page }) => {
    await openFixtureFile(page);

    // A plain "note"-intent annotation (the default).
    await page.keyboard.press("a");
    const noteBody = "D5 plain note for e2e";
    await page.locator("[data-kbc-annot-body-input]").fill(noteBody);
    await page.locator("[data-kbc-annot-save]").click();
    await expect(page.locator(".kbc-annotations__thread", { hasText: noteBody })).toBeVisible();

    // A flag-for-agent annotation.
    await page.keyboard.press("a");
    const flagBody = "D5 flagged comment for e2e";
    await page.locator("[data-kbc-annot-body-input]").fill(flagBody);
    await page.locator("[data-kbc-annot-intent-select]").selectOption("flag-for-agent");
    await page.locator("[data-kbc-annot-save]").click();
    const flaggedThread = page.locator(".kbc-annotations__thread", { hasText: flagBody });
    await expect(flaggedThread).toBeVisible();
    await expect(flaggedThread.locator("[data-kbc-intent-chip]")).toHaveText("Flag for agent");

    // Filtering to "Flagged" hides the note, keeps the flagged item.
    await page.locator('[data-kbc-annot-filter="flag-for-agent"]').click();
    await expect(page.locator(".kbc-annotations__thread", { hasText: noteBody })).toHaveCount(0);
    await expect(flaggedThread).toBeVisible();

    // Back to "All" restores it.
    await page.locator('[data-kbc-annot-filter="all"]').click();
    await expect(page.locator(".kbc-annotations__thread", { hasText: noteBody })).toBeVisible();
  });
});

test.describe("annotations v2 — diff-anchored comments (Commit page)", () => {
  test("commenting on a NEW-side diff line renders under the file row with a sha chip", async ({ page }) => {
    const sha = await featureBranchHeadSha();
    await page.goto(`${BASE}/r/${REPO_NAME}/~commit/${sha}`);
    await expect(page.locator(`[data-kbc-filechange="${FEATURE_FILE}"]`)).toBeVisible();

    const diffPromise = page.waitForResponse((res) => res.url().includes("/api/diff?"));
    await page.locator(`[data-kbc-filechange="${FEATURE_FILE}"] [data-kbc-filechange-toggle]`).click();
    await diffPromise;

    // Line 2 of feature_x.rs is `fn feature_x() -> i32 {` — a pure "add"
    // line (the whole file is new), so its NEW-side gutter number is
    // commentable.
    await page.locator('[data-kbc-diff-comment-line="2"]').click();
    const composer = page.locator('[data-kbc-diffcomment-composer="2"]');
    await expect(composer).toBeVisible();

    const body = "D5 diff comment for e2e";
    await composer.locator("[data-kbc-diffcomment-body]").fill(body);
    await composer.locator("[data-kbc-diffcomment-save]").click();

    // The composer closes on success…
    await expect(page.locator('[data-kbc-diffcomment-composer="2"]')).toHaveCount(0);
    // …and the comment renders under the file row as a compact thread,
    // carrying a short-sha chip pinned to THIS commit.
    const thread = page.locator("[data-kbc-diffannot-thread]", { hasText: body });
    await expect(thread).toBeVisible();
    await expect(thread.locator("[data-kbc-diffannot-sha]")).toHaveText(sha.slice(0, 7));
  });
});
