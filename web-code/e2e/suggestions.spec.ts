import { execFileSync } from "node:child_process";
import { readFileSync, writeFileSync } from "node:fs";
import { join } from "node:path";
import { expect, test } from "@playwright/test";
import { BASE, REPO_DIR, REPO_NAME } from "./helpers";

/// V4.S2 — in-browser suggestion editor + apply. Creates a disposable
/// branch, stays checked-out on it so apply can splice the working tree,
/// then restores main. Does NOT run Playwright in this builder — the
/// orchestrator gates that; this file is typechecked with the e2e tsc.

const SUGGEST_BRANCH = "e2e-suggestions";
const FILE = "README.md";
const TARGET = "e2e-suggest-target-line";
const REPLACEMENT_A = "e2e-suggest-replacement-a";
const REPLACEMENT_B = "e2e-suggest-replacement-b";

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

test.describe("suggestion editor + apply (V4.S2)", () => {
  test.afterAll(() => {
    tryGit(["checkout", "-f", "main"]);
    tryGit(["branch", "-D", SUGGEST_BRANCH]);
    try {
      writeFileSync(join(REPO_DIR, FILE), readmeWith(""));
    } catch {
      // restore best-effort
    }
  });

  test("edit, preview, apply, already_applied, drift 409, loopback 404", async ({
    page,
    request,
  }) => {
    test.skip(!REPO_DIR, "KB_CODE_E2E_REPO_DIR not set — global-setup didn't run");

    tryGit(["checkout", "-f", "main"]);
    tryGit(["branch", "-D", SUGGEST_BRANCH]);
    git(["checkout", "-q", "-b", SUGGEST_BRANCH]);
    writeFileSync(join(REPO_DIR, FILE), readmeWith(TARGET));
    git(["add", FILE]);
    git(["commit", "-q", "-m", "e2e suggestions: add target line"]);
    // Stay on SUGGEST_BRANCH so apply splices a file that exists in the WT.

    const createRes = await request.post(`${BASE}/api/reviews`, {
      data: {
        repo: REPO_NAME,
        head_ref: SUGGEST_BRANCH,
        base_ref: "main",
        title: "e2e suggestions",
      },
    });
    expect(createRes.ok(), `create review: ${createRes.status()} ${await createRes.text()}`).toBeTruthy();
    const created = (await createRes.json()) as { id: number };
    const reviewId = created.id;

    await page.goto(`${BASE}/r/${REPO_NAME}/~reviews/${reviewId}`);
    await expect(page.locator("[data-kbc-review-files]")).toBeVisible({ timeout: 10_000 });
    await page.locator(`[data-kbc-review-file-row="${FILE}"]`).click();
    const diff = page.locator(`[data-kbc-review-file-diff="${FILE}"]`);
    await expect(diff).toBeVisible({ timeout: 10_000 });

    // --- compose two threads on the same new-side line --------------------
    await diff.locator("[data-kbc-review-compose-new]").last().click();
    const composer = page.locator("[data-kbc-review-composer]");
    await expect(composer).toBeVisible();
    await composer.locator("[data-kbc-review-composer-body]").fill("first suggestion thread");
    await composer.locator("[data-kbc-review-composer-submit]").click();
    const threadA = diff.locator("[data-kbc-review-thread]").first();
    await expect(threadA).toBeVisible({ timeout: 10_000 });
    await expect(threadA).toContainText("first suggestion thread");
    const idA = await threadA.getAttribute("data-kbc-review-thread");
    // threadA above is a LIVE .first() locator — once a second thread
    // renders (line-bucket order, not creation order) it would re-resolve
    // to the wrong thread. Rebind by id for everything that follows.
    const threadARef = page
      .locator(`[data-kbc-review-file-diff="README.md"]`)
      .locator(`[data-kbc-review-thread="${idA}"]`);
    expect(idA).toBeTruthy();

    await diff.locator("[data-kbc-review-compose-new]").last().click();
    const composer2 = page.locator("[data-kbc-review-composer]");
    await expect(composer2).toBeVisible();
    await composer2.locator("[data-kbc-review-composer-body]").fill("second suggestion thread");
    await composer2.locator("[data-kbc-review-composer-submit]").click();
    // Threads render in line-bucket order, not creation order — select by
    // text, never by index (first-run flake).
    const threadB = diff
      .locator("[data-kbc-review-thread]")
      .filter({ hasText: "second suggestion thread" });
    await expect(threadB).toBeVisible({ timeout: 10_000 });
    const idB = await threadB.getAttribute("data-kbc-review-thread");
    expect(idB).toBeTruthy();
    expect(idB).not.toBe(idA);

    // --- open editor, live preview, save ----------------------------------
    await threadARef.locator(`[data-kbc-suggestion-open="${idA}"]`).click();
    const editor = threadARef.locator(`[data-kbc-suggestion-editor="${idA}"]`);
    await expect(editor).toBeVisible();
    const cm = editor.locator("[data-kbc-suggestion-cm] .cm-content");
    await expect(cm).toBeVisible();
    await cm.click();
    await page.keyboard.press("Control+A");
    await page.keyboard.type(REPLACEMENT_A);
    await expect(editor.locator("[data-kbc-suggestion-preview] .kbc-diff__hunk-header")).toContainText(
      "@@",
    );
    await expect(editor.locator("[data-kbc-suggestion-preview] .kbc-diff__line--add")).toContainText(
      REPLACEMENT_A,
    );
    await editor.locator(`[data-kbc-suggestion-save="${idA}"]`).click();
    const blockA = threadARef.locator("[data-kbc-review-thread-suggestion]");
    await expect(blockA).toBeVisible({ timeout: 10_000 });
    await expect(blockA.locator("[data-kbc-suggestion-preview]")).toContainText(REPLACEMENT_A);
    await expect(threadARef.locator(`[data-kbc-suggestion-edit="${idA}"]`)).toBeVisible();
    await expect(threadARef.locator(`[data-kbc-suggestion-remove="${idA}"]`)).toBeVisible();
    await expect(threadARef.locator(`[data-kbc-suggestion-apply="${idA}"]`)).toBeVisible();

    // --- apply → confirm → chip + working-tree file actually changed ------
    await threadARef.locator(`[data-kbc-suggestion-apply="${idA}"]`).click();
    await expect(page.locator(".confirm__go")).toBeVisible();
    await page.locator(".confirm__go").click();
    await expect(threadARef.locator("[data-kbc-suggestion-applied]")).toBeVisible({ timeout: 10_000 });
    await expect(page.locator('[data-kbc-toast="ok"]').last()).toContainText(/applied/i);

    const fileRes = await request.get(`${BASE}/api/file`, {
      params: { repo: REPO_NAME, path: FILE },
    });
    expect(fileRes.ok(), `GET /api/file: ${fileRes.status()}`).toBeTruthy();
    const fileBody = (await fileRes.json()) as { content: string };
    expect(fileBody.content).toContain(REPLACEMENT_A);
    expect(fileBody.content).not.toContain(TARGET);
    const afterFirst = fileBody.content;

    // --- apply again (re-PUT resets applied) → already_applied, no error --
    await threadARef.locator(`[data-kbc-suggestion-edit="${idA}"]`).click();
    await expect(threadARef.locator(`[data-kbc-suggestion-editor="${idA}"]`)).toBeVisible();
    await threadARef.locator(`[data-kbc-suggestion-save="${idA}"]`).click();
    await expect(threadARef.locator(`[data-kbc-suggestion-apply="${idA}"]`)).toBeEnabled({
      timeout: 10_000,
    });
    await threadARef.locator(`[data-kbc-suggestion-apply="${idA}"]`).click();
    await expect(page.locator(".confirm__go")).toBeVisible();
    await page.locator(".confirm__go").click();
    await expect(page.locator('[data-kbc-toast="ok"]').last()).toContainText(/already applied/i, {
      timeout: 10_000,
    });
    await expect(page.locator('[data-kbc-toast="err"]')).toHaveCount(0);

    // --- second suggestion on the same line, apply → 409 drift ------------
    await threadB.locator(`[data-kbc-suggestion-open="${idB}"]`).click();
    const editorB = threadB.locator(`[data-kbc-suggestion-editor="${idB}"]`);
    await expect(editorB).toBeVisible();
    const cmB = editorB.locator("[data-kbc-suggestion-cm] .cm-content");
    await cmB.click();
    await page.keyboard.press("Control+A");
    await page.keyboard.type(REPLACEMENT_B);
    await editorB.locator(`[data-kbc-suggestion-save="${idB}"]`).click();
    await expect(threadB.locator(`[data-kbc-suggestion-apply="${idB}"]`)).toBeVisible({
      timeout: 10_000,
    });
    await threadB.locator(`[data-kbc-suggestion-apply="${idB}"]`).click();
    await expect(page.locator(".confirm__go")).toBeVisible();
    await page.locator(".confirm__go").click();
    await expect(page.locator('[data-kbc-toast="err"]')).toContainText(/Can't apply/i, {
      timeout: 10_000,
    });

    const fileRes2 = await request.get(`${BASE}/api/file`, {
      params: { repo: REPO_NAME, path: FILE },
    });
    expect(fileRes2.ok()).toBeTruthy();
    const fileBody2 = (await fileRes2.json()) as { content: string };
    expect(fileBody2.content).toBe(afterFirst);
    expect(fileBody2.content).not.toContain(REPLACEMENT_B);
    expect(readFileSync(join(REPO_DIR, FILE), "utf-8")).toBe(afterFirst);

    // --- intercept POST apply → 404 replaces the button with the hint -----
    await page.route("**/api/annotations/*/apply", async (route) => {
      if (route.request().method() === "POST") {
        await route.fulfill({ status: 404, body: "Not Found" });
        return;
      }
      await route.continue();
    });
    // Re-PUT thread A so Apply is enabled again (applied was reset above
    // already; a 404 latch is what we want).
    const applyBtn = threadARef.locator(`[data-kbc-suggestion-apply="${idA}"]`);
    if (await applyBtn.count()) {
      await applyBtn.click();
      await expect(page.locator(".confirm__go")).toBeVisible();
      await page.locator(".confirm__go").click();
    }
    await expect(threadARef.locator("[data-kbc-suggestion-loopback]")).toBeVisible({ timeout: 10_000 });
    await expect(threadARef.locator("[data-kbc-suggestion-loopback]")).toContainText(
      "Apply requires a loopback session",
    );
    await expect(threadARef.locator(`[data-kbc-suggestion-apply="${idA}"]`)).toHaveCount(0);
  });
});
