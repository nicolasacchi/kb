import { expect, test } from "@playwright/test";
import { FEATURE_BRANCH, FEATURE_FILE } from "./fixture-repo";
import { BASE, REPO_DIR, REPO_NAME } from "./helpers";
import { execFileSync } from "node:child_process";

// V76-C1 — a Ruby fence in a review comment paints `.kbc-hl-*` (the
// reader's class table; the brief's `.tok-*` name is not used), and a
// new-file hunk is painted. Unit: V76-C1. The DOM hook `data-kbc-hl` is
// the existing painted-span contract (`diff-syntax.spec.ts`).

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

const FENCE_BODY = ["look:", "", "```ruby", "def greet(name)", '  "hi"', "end", "```"].join("\n");

test.describe("highlight/1 snippet paint (V76-C1)", () => {
  test("a comment with a Ruby fence renders .kbc-hl-* / data-kbc-hl", async ({ page, request }) => {
    test.skip(!REPO_DIR, "KB_CODE_E2E_REPO_DIR not set — global-setup didn't run");

    const createRes = await request.post(`${BASE}/api/reviews`, {
      data: {
        repo: REPO_NAME,
        head_ref: FEATURE_BRANCH,
        base_ref: "main",
        title: "e2e highlight fence",
      },
    });
    expect(createRes.ok(), `create review: ${createRes.status()} ${await createRes.text()}`).toBeTruthy();
    const created = (await createRes.json()) as { id: number };
    const reviewId = created.id;

    const hl = page.waitForResponse(
      (res) => res.url().includes("/api/highlight") && res.request().method() === "POST" && res.ok(),
      { timeout: 20_000 },
    );

    await page.goto(`${BASE}/r/${REPO_NAME}/~reviews/${reviewId}`);
    await expect(page.locator("[data-kbc-review-title]")).toContainText("e2e highlight fence");
    await page.locator(`[data-kbc-review-file-row="${FEATURE_FILE}"]`).click();
    const featDiff = page.locator(`[data-kbc-review-file-diff="${FEATURE_FILE}"]`);
    await expect(featDiff).toBeVisible({ timeout: 10_000 });
    await featDiff.locator("[data-kbc-review-compose-new]").first().click();
    const composer = page.locator("[data-kbc-review-composer]");
    await expect(composer).toBeVisible();
    await composer.locator("[data-kbc-review-composer-body]").fill(FENCE_BODY);
    await composer.locator("[data-kbc-review-composer-submit]").click();
    const thread = featDiff.locator("[data-kbc-review-thread]").first();
    await expect(thread).toBeVisible({ timeout: 10_000 });
    await hl.catch(() => undefined);
    await expect(thread.locator("[data-kbc-hl]").first()).toBeVisible({ timeout: 15_000 });
    await expect(thread.locator(".kbc-hl-keyword").first()).toBeVisible();
  });

  test("a new file's hunk is painted", async ({ page, request }) => {
    test.skip(!REPO_DIR, "KB_CODE_E2E_REPO_DIR not set — global-setup didn't run");
    tryGit(["checkout", "main"]);

    const createRes = await request.post(`${BASE}/api/reviews`, {
      data: {
        repo: REPO_NAME,
        head_ref: FEATURE_BRANCH,
        base_ref: "main",
        title: "e2e highlight new-file",
      },
    });
    expect(createRes.ok(), `create review: ${createRes.status()} ${await createRes.text()}`).toBeTruthy();
    const created = (await createRes.json()) as { id: number };

    await page.goto(`${BASE}/r/${REPO_NAME}/~reviews/${created.id}`);
    await page.locator(`[data-kbc-review-file-row="${FEATURE_FILE}"]`).click();
    const featDiff = page.locator(`[data-kbc-review-file-diff="${FEATURE_FILE}"]`);
    await expect(featDiff).toBeVisible({ timeout: 10_000 });
    await expect(featDiff.locator(".kbc-diff__line--add [data-kbc-hl]").first()).toBeVisible({
      timeout: 15_000,
    });
  });
});
