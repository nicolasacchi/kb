import { expect, test } from "@playwright/test";
import { STORY_COMMIT_2_SUBJECT, STORY_FILE } from "./fixture-repo";
import { BASE, REPO_NAME } from "./helpers";

/// V4.D2 — syntax-highlighted diff lines on a Commit-page rust file.
/// Written, not run in this phase. Uses `story.rs`'s second commit
/// (`STORY_COMMIT_2_SUBJECT`) so the hunk has both context (`tell_story`)
/// and add (`continue_story`) lines — an add-only first-commit file
/// would not exercise the context assertion.

interface FileHistoryBody {
  entries: Array<{ sha: string; subject: string }>;
}

async function storyExpandSha(): Promise<string> {
  const res = await fetch(`${BASE}/api/file-history?repo=${REPO_NAME}&path=${STORY_FILE}`);
  const body = (await res.json()) as FileHistoryBody;
  const hit = body.entries.find((e) => e.subject === STORY_COMMIT_2_SUBJECT);
  if (!hit) throw new Error(`fixture bug: ${STORY_COMMIT_2_SUBJECT} not in file-history`);
  return hit.sha;
}

async function expandStoryDiff(page: import("@playwright/test").Page): Promise<void> {
  const diffPromise = page.waitForResponse((res) => res.url().includes("/api/diff?"));
  await page.locator(`[data-kbc-filechange="${STORY_FILE}"] [data-kbc-filechange-toggle]`).click();
  await diffPromise;
}

test.describe("diff syntax highlighting (V4.D2)", () => {
  test("token spans on add and context lines", async ({ page }) => {
    const sha = await storyExpandSha();
    await page.goto(`${BASE}/r/${REPO_NAME}/~commit/${sha}`);
    await expect(page.locator(`[data-kbc-filechange="${STORY_FILE}"]`)).toBeVisible();

    const filePromise = page.waitForResponse(
      (res) => res.url().includes("/api/file?") && res.ok(),
    );
    await expandStoryDiff(page);
    await filePromise;

    await expect(page.locator(".kbc-diff__line--add [data-kbc-hl]").first()).toBeVisible({
      timeout: 10_000,
    });
    await expect(page.locator(".kbc-diff__line--add [data-kbc-hl]")).not.toHaveCount(0);
    // Context lines: `.kbc-diff__line` without add/remove modifiers.
    await expect(
      page.locator(".kbc-diff__line:not(.kbc-diff__line--add):not(.kbc-diff__line--remove) [data-kbc-hl]"),
    ).not.toHaveCount(0);
  });

  test("GET /api/file 404 still renders plain, no error toast", async ({ page }) => {
    const sha = await storyExpandSha();
    await page.route("**/api/file**", (route) =>
      route.fulfill({
        status: 404,
        contentType: "application/json",
        body: JSON.stringify({ error: "not found" }),
      }),
    );
    await page.goto(`${BASE}/r/${REPO_NAME}/~commit/${sha}`);
    await expect(page.locator(`[data-kbc-filechange="${STORY_FILE}"]`)).toBeVisible();
    await expandStoryDiff(page);

    await expect(page.locator(".kbc-diff__line").first()).toBeVisible();
    await expect(page.locator("[data-kbc-hl]")).toHaveCount(0);
    await expect(page.locator('[data-kbc-toast="err"]')).toHaveCount(0);
  });

  test("pref off (localStorage seed) paints plain", async ({ page }) => {
    const sha = await storyExpandSha();
    await page.addInitScript(() => {
      localStorage.setItem("kbc:prefs", JSON.stringify({ diffSyntaxHighlight: false }));
    });
    await page.goto(`${BASE}/r/${REPO_NAME}/~commit/${sha}`);
    await expect(page.locator(`[data-kbc-filechange="${STORY_FILE}"]`)).toBeVisible();
    await expandStoryDiff(page);

    await expect(page.locator(".kbc-diff__line").first()).toBeVisible();
    await expect(page.locator("[data-kbc-hl]")).toHaveCount(0);
  });
});
