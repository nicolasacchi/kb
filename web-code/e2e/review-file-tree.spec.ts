import { execFileSync } from "node:child_process";
import { mkdirSync, writeFileSync } from "node:fs";
import { join } from "node:path";
import { expect, test } from "@playwright/test";
import { FEATURE_BRANCH } from "./fixture-repo";
import { BASE, REPO_DIR, REPO_NAME } from "./helpers";

/// V76-R2b — the review file tree: folders + kind icons, click opens the
/// first hunk, map pane resize persists. Disposable branch so it never
/// touches main's tip or the fixture's recorded tree-cursor counts.

const TREE_BRANCH = "e2e-rdiff-tree";
const ADDED = "app/models/e2e_tree_added.rb";
const OTHER = "lib/e2e_tree_other.rs";

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

test.describe("review file tree (V76-R2b)", () => {
  test.afterAll(() => {
    tryGit(["checkout", "main"]);
    tryGit(["branch", "-D", TREE_BRANCH]);
  });

  test("folders + icons; click a file → first hunk in view; resize persists", async ({
    page,
    request,
  }) => {
    test.skip(!REPO_DIR, "KB_CODE_E2E_REPO_DIR not set — global-setup didn't run");

    tryGit(["checkout", "main"]);
    tryGit(["branch", "-D", TREE_BRANCH]);
    git(["checkout", "-q", "-b", TREE_BRANCH, FEATURE_BRANCH]);
    mkdirSync(join(REPO_DIR, "app/models"), { recursive: true });
    mkdirSync(join(REPO_DIR, "lib"), { recursive: true });
    writeFileSync(
      join(REPO_DIR, ADDED),
      Array.from({ length: 24 }, (_, i) => `puts "e2e tree line ${i + 1}"`).join("\n") + "\n",
    );
    writeFileSync(join(REPO_DIR, OTHER), "fn e2e_tree_other() -> i32 { 1 }\n");
    git(["add", ADDED, OTHER]);
    git(["commit", "-q", "-m", "e2e review file tree nested files"]);
    git(["checkout", "-q", "main"]);

    const createRes = await request.post(`${BASE}/api/reviews`, {
      data: { repo: REPO_NAME, head_ref: TREE_BRANCH, base_ref: "main", title: "e2e file tree" },
    });
    expect(createRes.ok(), `create: ${createRes.status()} ${await createRes.text()}`).toBeTruthy();
    const reviewId = ((await createRes.json()) as { id: number }).id;

    await page.goto(`${BASE}/r/${REPO_NAME}/~reviews/${reviewId}/diff`);
    await expect(page.locator("[data-kbc-rdiff]")).toBeVisible({ timeout: 15_000 });
    const map = page.locator("[data-kbc-rdiff-map]");
    await expect(map).toBeVisible({ timeout: 10_000 });

    await expect(page.locator("[data-kbc-rdiff-map-folder='app']")).toBeVisible();
    await expect(page.locator("[data-kbc-rdiff-map-folder='app/models']")).toBeVisible();
    await expect(page.locator(`[data-kbc-rdiff-map-row="${ADDED}"]`)).toBeVisible();
    await expect(page.locator('[data-kbc-file-kind="ruby"]')).toBeVisible();
    await expect(page.locator('[data-kbc-file-kind="rust"]').first()).toBeVisible();

    const row = page.locator(`[data-kbc-rdiff-map-row="${ADDED}"]`);
    await row.click();
    await expect(page).toHaveURL(new RegExp(`/diff/${ADDED.replace(/\//g, "/")}.*file=`));
    const section = page.locator(`[data-kbc-rdiff-file="${ADDED}"]`);
    await expect(section).toBeVisible({ timeout: 10_000 });
    const hunk = section.locator("[data-kbc-hunk], [data-kbc-sdiff-row='hunk']").first();
    await expect(hunk).toBeVisible({ timeout: 10_000 });
    const hunkBox = await hunk.boundingBox();
    expect(hunkBox).not.toBeNull();
    if (hunkBox) {
      expect(hunkBox.y).toBeGreaterThanOrEqual(0);
      expect(hunkBox.y).toBeLessThan(720);
    }
    // Added-file body is not an empty hatch: at least one add line of text.
    await expect(section.locator(".kbc-diff__text, .kbc-sdiff__text").first()).toBeVisible();

    const sep = page.locator("[data-kbc-rdiff-map-sep]");
    await expect(sep).toBeVisible();
    const before = await sep.boundingBox();
    expect(before).not.toBeNull();
    if (before) {
      await page.mouse.move(before.x + before.width / 2, before.y + 20);
      await page.mouse.down();
      await page.mouse.move(before.x + 80, before.y + 20, { steps: 8 });
      await page.mouse.up();
    }
    const stored = await page.evaluate(() => localStorage.getItem("kbc:review-map-width"));
    expect(stored).toBeTruthy();
    await page.reload();
    await expect(page.locator("[data-kbc-rdiff-map]")).toBeVisible({ timeout: 10_000 });
    const storedAfter = await page.evaluate(() => localStorage.getItem("kbc:review-map-width"));
    expect(storedAfter).toBe(stored);
  });
});
