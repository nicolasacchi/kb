import { execFileSync } from "node:child_process";
import { writeFileSync } from "node:fs";
import { join } from "node:path";
import { expect, test } from "@playwright/test";
import { BASE, REPO_DIR, REPO_NAME } from "./helpers";

/// V76-R2c — collapse-on-tick + token-level suggestion preview.
/// Playwright is not run in this builder; CI's `code-e2e` job is the gate.
///
/// Hooks this spec owns: `data-kbc-hunk-collapsed`, `data-kbc-sugdiff-tok`,
/// `data-kbc-suggestion-apply-preview`. Existing `data-kbc-hunk-viewed-toggle`,
/// `data-kbc-suggestion-preview`, `data-kbc-suggestion-apply`, `.confirm__go`
/// stay as review-diff-v2 / suggestions.spec already assert them.

const BRANCH = "e2e-r2c";
const FILE = "README.md";
const TARGET = "e2e-r2c-target-line";
const REPLACEMENT = "e2e-r2c-target-line #changed";

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

function readmeWith(extra: string): string {
  return `# fixture\n\nA fixture repo for kb-code's e2e suite.\n${extra}\n`;
}

test.describe("review diff collapse-on-tick + suggestion tokens (V76-R2c)", () => {
  test.afterAll(() => {
    tryGit(["checkout", "-f", "main"]);
    tryGit(["branch", "-D", BRANCH]);
    try {
      writeFileSync(join(REPO_DIR, FILE), readmeWith(""));
    } catch {
      // restore best-effort
    }
  });

  test("tick a hunk collapses it; a suggestion emphasises tokens; apply preview shows the diff", async ({
    page,
    request,
  }) => {
    test.skip(!REPO_DIR, "KB_CODE_E2E_REPO_DIR not set — global-setup didn't run");

    tryGit(["checkout", "-f", "main"]);
    tryGit(["branch", "-D", BRANCH]);
    git(["checkout", "-q", "-b", BRANCH]);
    writeFileSync(join(REPO_DIR, FILE), readmeWith(TARGET));
    git(["add", FILE]);
    git(["commit", "-q", "-m", "e2e r2c: add target line"]);

    const createRes = await request.post(`${BASE}/api/reviews`, {
      data: {
        repo: REPO_NAME,
        head_ref: BRANCH,
        base_ref: "main",
        title: "e2e r2c collapse and tokens",
      },
    });
    expect(createRes.ok(), `create review: ${createRes.status()} ${await createRes.text()}`).toBeTruthy();
    const created = (await createRes.json()) as { id: number };
    const reviewId = created.id;

    await page.goto(`${BASE}/r/${REPO_NAME}/~reviews/${reviewId}/diff`);
    await expect(page.locator("[data-kbc-rdiff]")).toBeVisible({ timeout: 15_000 });

    const firstStrip = page.locator("[data-kbc-hunk]").first();
    await expect(firstStrip).toBeVisible({ timeout: 15_000 });
    const hunkId = await firstStrip.getAttribute("data-kbc-hunk");
    expect(hunkId).toBeTruthy();
    await page.locator(`[data-kbc-hunk-viewed-toggle="${hunkId}"]`).click();
    await expect(firstStrip).toHaveAttribute("data-kbc-hunk-viewed", "1", { timeout: 10_000 });
    await expect(firstStrip).toHaveAttribute("data-kbc-hunk-collapsed", "1");

    await page.goto(`${BASE}/r/${REPO_NAME}/~reviews/${reviewId}`);
    await expect(page.locator("[data-kbc-review-files]")).toBeVisible({ timeout: 10_000 });
    await page.locator(`[data-kbc-review-file-row="${FILE}"]`).click();
    const diff = page.locator(`[data-kbc-review-file-diff="${FILE}"]`);
    await expect(diff).toBeVisible({ timeout: 10_000 });
    await diff.locator("[data-kbc-review-compose-new]").last().click();
    const composer = page.locator("[data-kbc-review-composer]");
    await expect(composer).toBeVisible();
    await composer.locator("[data-kbc-review-composer-body]").fill("r2c token suggestion");
    await composer.locator("[data-kbc-review-composer-submit]").click();
    const thread = page.locator("[data-kbc-review-thread]").filter({ hasText: "r2c token suggestion" });
    await expect(thread).toBeVisible({ timeout: 10_000 });
    const threadId = await thread.getAttribute("data-kbc-review-thread");
    expect(threadId).toBeTruthy();
    await thread.locator(`[data-kbc-suggestion-open="${threadId}"]`).click();
    const editor = thread.locator(`[data-kbc-suggestion-editor="${threadId}"]`);
    await expect(editor).toBeVisible();
    const cm = editor.locator("[data-kbc-suggestion-cm] .cm-content");
    await cm.click();
    await page.keyboard.press("Control+a");
    await page.keyboard.type(REPLACEMENT);
    await editor.locator(`[data-kbc-suggestion-save="${threadId}"]`).click();
    const block = thread.locator("[data-kbc-review-thread-suggestion]");
    await expect(block).toBeVisible({ timeout: 10_000 });
    await expect(block.locator("[data-kbc-suggestion-preview]")).toContainText(REPLACEMENT);
    await expect(block.locator('[data-kbc-sugdiff-tok="add"]').first()).toBeVisible();
    await expect(block.locator("[data-kbc-sugdiff-caption]")).toContainText("tokens");

    await thread.locator(`[data-kbc-suggestion-apply="${threadId}"]`).click();
    await expect(page.locator(".confirm__go")).toBeVisible();
    await expect(page.locator("[data-kbc-suggestion-apply-preview]")).toBeVisible();
    await expect(page.locator("[data-kbc-suggestion-apply-preview] [data-kbc-sugdiff]")).toBeVisible();
    await expect(page.locator("[data-kbc-suggestion-apply-preview] [data-kbc-sugdiff-caption]")).toBeVisible();
    await page.locator(".confirm__cancel").click();
  });
});
