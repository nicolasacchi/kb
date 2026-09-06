import { execFileSync } from "node:child_process";
import { writeFileSync } from "node:fs";
import { join } from "node:path";
import { expect, test } from "@playwright/test";
import { FEATURE_BRANCH, FEATURE_FILE } from "./fixture-repo";
import { BASE, REPO_DIR, REPO_NAME } from "./helpers";

/// V4.D1 — unified ↔ split toggle on a Commit-page file row.
/// Follows the annotations2.spec.ts Commit-page fixture pattern
/// (`FEATURE_FILE` on a non-root commit) and adds a restore-guarded
/// side-branch commit so the same file has a balanced replace + an
/// extra add (paired rows + spacer cells). Does not edit existing specs.

const SPLIT_BRANCH = "e2e-diff-split";

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

async function expandFileDiff(page: import("@playwright/test").Page) {
  const diffPromise = page.waitForResponse((res) => res.url().includes("/api/diff?"));
  await page.locator(`[data-kbc-filechange="${FEATURE_FILE}"] [data-kbc-filechange-toggle]`).click();
  await diffPromise;
}

test.describe("diff split view (V4.D1)", () => {
  let splitSha: string;

  test.beforeAll(() => {
    test.skip(!REPO_DIR, "KB_CODE_E2E_REPO_DIR not set — global-setup didn't run");
    tryGit(["checkout", "main"]);
    tryGit(["branch", "-D", SPLIT_BRANCH]);
    git(["checkout", "-q", "-b", SPLIT_BRANCH, FEATURE_BRANCH]);
    writeFileSync(
      join(REPO_DIR, FEATURE_FILE),
      ["// added on feature-x", "fn feature_x() -> u32 {", "    42", "    extra", "}", ""].join("\n"),
    );
    git(["add", FEATURE_FILE]);
    git(["commit", "-q", "-m", "e2e split-view replace"]);
    splitSha = git(["rev-parse", "HEAD"]);
    git(["checkout", "-q", "main"]);
  });

  test.afterAll(() => {
    tryGit(["checkout", "main"]);
    tryGit(["branch", "-D", SPLIT_BRANCH]);
  });

  test("toggle, paired replace, spacers, persist, unified comments", async ({ page }) => {
    await page.goto(`${BASE}/r/${REPO_NAME}/~commit/${splitSha}`);
    await expect(page.locator(`[data-kbc-filechange="${FEATURE_FILE}"]`)).toBeVisible();
    await expandFileDiff(page);

    // Default is unified: no split grid, comment button present.
    await expect(page.locator("[data-kbc-sdiff]")).toHaveCount(0);
    await expect(page.locator('[data-kbc-diff-comment-line="2"]')).toBeVisible();
    await expect(page.locator('[data-kbc-diff-mode="unified"]')).toBeVisible();

    // Toggle unified → split.
    await page.locator('[data-kbc-diff-mode-toggle="split"]').click();
    await expect(page.locator("[data-kbc-sdiff]")).toBeVisible();
    await expect(page.locator('[data-kbc-diff-mode="split"]')).toBeVisible();
    await expect(page.locator("[data-kbc-sdiff] [data-kbc-diff-comment-line]")).toHaveCount(0);

    const pairs = page.locator('[data-kbc-sdiff-row="pair"]');
    await expect(pairs).toHaveCount(5);
    // Context both sides.
    await expect(pairs.nth(0).locator(".kbc-sdiff__old")).toContainText("// added on feature-x");
    await expect(pairs.nth(0).locator(".kbc-sdiff__new")).toContainText("// added on feature-x");
    // Balanced replace: old + new side by side.
    await expect(pairs.nth(1).locator(".kbc-sdiff__old")).toContainText("i32");
    await expect(pairs.nth(1).locator(".kbc-sdiff__new")).toContainText("u32");
    await expect(pairs.nth(2).locator(".kbc-sdiff__old")).toContainText("1");
    await expect(pairs.nth(2).locator(".kbc-sdiff__new")).toContainText("42");
    // Unbalanced extra add → spacer on the old side.
    await expect(pairs.nth(3).locator(".kbc-sdiff__old")).toHaveClass(/kbc-sdiff__spacer/);
    await expect(pairs.nth(3).locator(".kbc-sdiff__new")).toContainText("extra");

    const stored = await page.evaluate(() => localStorage.getItem("kbc:prefs"));
    expect(stored).toBeTruthy();
    expect(JSON.parse(stored as string).diffMode).toBe("split");

    // Pref survives reload (re-expand: expanded is component state).
    await page.reload();
    await expect(page.locator(`[data-kbc-filechange="${FEATURE_FILE}"]`)).toBeVisible();
    await expandFileDiff(page);
    await expect(page.locator("[data-kbc-sdiff]")).toBeVisible();
    await expect(page.locator('[data-kbc-diff-mode="split"]')).toBeVisible();

    // Toggle back: comment button still present + opens the composer.
    await page.locator('[data-kbc-diff-mode-toggle="unified"]').click();
    await expect(page.locator("[data-kbc-sdiff]")).toHaveCount(0);
    const commentBtn = page.locator('[data-kbc-diff-comment-line="2"]');
    await expect(commentBtn).toBeVisible();
    await commentBtn.click();
    await expect(page.locator('[data-kbc-diffcomment-composer="2"]')).toBeVisible();
    await page.locator("[data-kbc-diffcomment-cancel]").click();
    await expect(page.locator('[data-kbc-diffcomment-composer="2"]')).toHaveCount(0);
  });
});
