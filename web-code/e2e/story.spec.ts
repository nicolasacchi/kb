import { expect, test } from "@playwright/test";
import { STORY_COMMIT_2_SUBJECT, STORY_FILE } from "./fixture-repo";
import { BASE, REPO_NAME } from "./helpers";

/// Phase C7 ("kb-code v2 — The Operable Reader") end to end: story mode —
/// replay a file's evolution commit by commit. Drives `story.rs`
/// (`fixture-repo.ts`'s own two-commit-on-main addition, added specifically
/// for this spec — see that file's doc for why it had to land on `main`
/// itself, and the blast radius that had on `time.spec.ts`'s `mainHeadSha`/
/// branches-page assertions).

interface FileHistoryBody {
  entries: Array<{ sha: string }>;
}

/// `story.rs`'s own two commits, newest-first (mirrors `file-history/1`'s
/// wire order) — discovered live rather than hardcoded, same discipline as
/// `time.spec.ts`'s `initialCommitSha`.
async function storyFileHistory(): Promise<Array<{ sha: string }>> {
  const res = await fetch(`${BASE}/api/file-history?repo=${REPO_NAME}&path=${STORY_FILE}`);
  const body = (await res.json()) as FileHistoryBody;
  return body.entries;
}

test.describe("story mode", () => {
  test("plays story.rs's two commits, tints changed lines, and Esc exits pinned to the last step", async ({
    page,
  }) => {
    const entries = await storyFileHistory();
    expect(entries).toHaveLength(2);
    const newestSha = entries[0].sha;

    await page.goto(`${BASE}/r/${REPO_NAME}`);

    const historyPromise = page.waitForResponse((res) => res.url().includes("/api/file-history?"));
    await page.locator(".kbc-tree__row", { hasText: STORY_FILE }).click();
    await expect(page.locator(".kbc-codeview")).toContainText("tell_story", { timeout: 10_000 });
    await historyPromise;

    await page.locator('[data-kbc-itab="history"]').click();
    await page.locator("[data-kbc-history-story]").click();
    await expect(page).toHaveURL(/~story/);

    // Step 1/2 — the oldest commit (the file's own from-scratch add).
    await expect(page.locator("[data-kbc-story-counter]")).toHaveText("1/2");
    await expect(page.locator(".kbc-story-line").first()).toBeVisible({ timeout: 10_000 });

    await page.keyboard.press("ArrowRight");

    // Step 2/2 — the newest commit, which ADDS `continue_story` on top of
    // the first commit's `tell_story` (a real "+" hunk, not a from-scratch
    // add) — the changed-line tint and narration both reflect it.
    await expect(page.locator("[data-kbc-story-counter]")).toHaveText("2/2");
    await expect(page.locator(".kbc-story-line").first()).toBeVisible({ timeout: 10_000 });
    await expect(page.locator("[data-kbc-story-narration-subject]")).toHaveText(STORY_COMMIT_2_SUBJECT);

    await page.keyboard.press("Escape");
    await expect(page).toHaveURL(new RegExp(`[?&]ref=${newestSha}(&|$)`));
    // Back in the plain reader — the story player has unmounted.
    await expect(page.locator("[data-kbc-story]")).toHaveCount(0);
    await expect(page.locator(".kbc-codeview")).toContainText("continue_story", { timeout: 10_000 });
  });
});
