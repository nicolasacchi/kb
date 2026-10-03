import { execFileSync } from "node:child_process";
import { writeFileSync } from "node:fs";
import { join } from "node:path";
import { expect, test } from "@playwright/test";
import { BASE, REPO_DIR, REPO_NAME } from "./helpers";

/// v0.44 F9/F9b (X9 coverage) — the two SPA surfaces that ride `GET
/// /api/reviews/{id}/since`:
///
///  1. the Room's one-click "Re-affirm approval on psN" button, offered only
///     when a verdict went stale because a LATER patchset landed that is a
///     pure rebase (no author hunk new or gone) on a MOVED base — and which,
///     when clicked, records an ordinary verdict on the new patchset;
///  2. the full-page diff's "Author changes only" switch on a `?ps=a..b`
///     range, which narrows the interdiff's tip-to-tip file list (upstream
///     files included) to the paths the author actually touched.
///
/// Each test builds its own disposable base + head branches (never `main`,
/// never `FEATURE_BRANCH`) with a SECOND patchset made through the snapshot
/// route, and removes them afterwards — the same pattern
/// `review-findings-touched.spec.ts` established.

const FILE_A = "e2e_since_a.rs";
const FILE_B = "e2e_since_b.rs";

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

function lines(tag: string, edits: Record<number, string> = {}): string {
  const out = Array.from({ length: 20 }, (_, i) => `// ${tag} line ${i + 1}`);
  for (const [n, text] of Object.entries(edits)) out[Number(n) - 1] = text;
  return `${out.join("\n")}\n`;
}

function write(file: string, body: string): void {
  writeFileSync(join(REPO_DIR, file), body);
}

/// base branch (two files) + head branch whose ps1 edits FILE_A line 3.
function seed(baseBranch: string, headBranch: string): void {
  tryGit(["checkout", "main"]);
  tryGit(["branch", "-D", headBranch]);
  tryGit(["branch", "-D", baseBranch]);
  git(["checkout", "-q", "-b", baseBranch, "main"]);
  write(FILE_A, lines("a"));
  write(FILE_B, lines("b"));
  git(["add", FILE_A, FILE_B]);
  git(["commit", "-q", "-m", "e2e since base"]);
  git(["checkout", "-q", "-b", headBranch, baseBranch]);
  write(FILE_A, lines("a", { 3: "// author edit one" }));
  git(["commit", "-aq", "-m", "e2e since ps1"]);
  git(["checkout", "-q", "main"]);
}

/// Move the base (an edit to FILE_B only), rebase the head onto it and
/// optionally add an author edit to FILE_A; leaves `main` checked out.
function rebaseOntoMovedBase(
  baseBranch: string,
  headBranch: string,
  authorEdit: boolean,
): void {
  git(["checkout", "-q", baseBranch]);
  write(FILE_B, lines("b", { 15: "// upstream edit" }));
  git(["commit", "-aq", "-m", "e2e since upstream move"]);
  git(["checkout", "-q", headBranch]);
  git(["rebase", "-q", baseBranch]);
  if (authorEdit) {
    write(FILE_A, lines("a", { 3: "// author edit one", 10: "// author edit two" }));
    git(["commit", "-aq", "-m", "e2e since ps2 author edit"]);
  }
  git(["checkout", "-q", "main"]);
}

test.describe("verdict re-affirm and the author-only switch (F9/F9b)", () => {
  const cleanup: Array<[string, string]> = [];
  test.afterAll(() => {
    tryGit(["checkout", "main"]);
    for (const [h, b] of cleanup) {
      tryGit(["branch", "-D", h]);
      tryGit(["branch", "-D", b]);
    }
  });

  test("a rebase-only patchset offers Re-affirm, and clicking it records the verdict on it", async ({
    page,
    request,
  }) => {
    test.skip(!REPO_DIR, "KB_CODE_E2E_REPO_DIR not set — global-setup didn't run");
    const baseBranch = "e2e-since-rf-base";
    const headBranch = "e2e-since-rf";
    cleanup.push([headBranch, baseBranch]);
    seed(baseBranch, headBranch);

    const createRes = await request.post(`${BASE}/api/reviews`, {
      data: { repo: REPO_NAME, head_ref: headBranch, base_ref: baseBranch, title: "e2e re-affirm" },
    });
    expect(createRes.ok(), `create review: ${createRes.status()} ${await createRes.text()}`).toBeTruthy();
    const reviewId = ((await createRes.json()) as { id: number }).id;

    const verdictRes = await request.put(`${BASE}/api/reviews/${reviewId}/verdict`, {
      data: { state: "approve" },
    });
    expect(verdictRes.ok(), `verdict: ${verdictRes.status()} ${await verdictRes.text()}`).toBeTruthy();

    // ps2 = the same change replayed onto a MOVED base: zero author hunks.
    rebaseOntoMovedBase(baseBranch, headBranch, false);
    const snapRes = await request.post(`${BASE}/api/reviews/${reviewId}/snapshot`, { data: {} });
    expect(snapRes.ok(), `snapshot: ${snapRes.status()} ${await snapRes.text()}`).toBeTruthy();

    await page.goto(`${BASE}/r/${REPO_NAME}/~reviews/${reviewId}`);
    await expect(page.locator("[data-kbc-review-verdict-bar]")).toBeVisible({ timeout: 15_000 });
    await expect(page.locator("[data-kbc-review-verdict-stale-hint]")).toBeVisible({
      timeout: 15_000,
    });
    const reaffirm = page.locator('[data-kbc-review-verdict-reaffirm="2"]');
    await expect(reaffirm).toBeVisible({ timeout: 15_000 });
    await expect(reaffirm).toContainText("Re-affirm approval on ps2");

    await reaffirm.click();
    // A human click recorded an ordinary verdict on ps2: no longer stale, so
    // the hint and the button both go away.
    await expect(page.locator("[data-kbc-review-verdict-stale-hint]")).toHaveCount(0, {
      timeout: 15_000,
    });
    await expect(page.locator("[data-kbc-review-verdict-reaffirm]")).toHaveCount(0);
    const after = await request.get(`${BASE}/api/reviews/${reviewId}`);
    expect(after.ok()).toBeTruthy();
    const body = (await after.json()) as { verdict?: { state: string; ps: number | null } };
    expect(body.verdict?.state).toBe("approve");
    expect(body.verdict?.ps).toBe(2);
  });

  test("the author-only switch narrows an interdiff to the paths the author touched", async ({
    page,
    request,
  }) => {
    test.skip(!REPO_DIR, "KB_CODE_E2E_REPO_DIR not set — global-setup didn't run");
    const baseBranch = "e2e-since-ao-base";
    const headBranch = "e2e-since-ao";
    cleanup.push([headBranch, baseBranch]);
    seed(baseBranch, headBranch);

    const createRes = await request.post(`${BASE}/api/reviews`, {
      data: { repo: REPO_NAME, head_ref: headBranch, base_ref: baseBranch, title: "e2e author-only" },
    });
    expect(createRes.ok(), `create review: ${createRes.status()} ${await createRes.text()}`).toBeTruthy();
    const reviewId = ((await createRes.json()) as { id: number }).id;

    // ps2 = rebased onto a base that moved FILE_B, plus a real author edit of
    // FILE_A: the tip-to-tip interdiff lists BOTH files, the author delta one.
    rebaseOntoMovedBase(baseBranch, headBranch, true);
    const snapRes = await request.post(`${BASE}/api/reviews/${reviewId}/snapshot`, { data: {} });
    expect(snapRes.ok(), `snapshot: ${snapRes.status()} ${await snapRes.text()}`).toBeTruthy();

    await page.goto(`${BASE}/r/${REPO_NAME}/~reviews/${reviewId}/diff?ps=1..2`);
    await expect(page.locator("[data-kbc-rdiff]")).toBeVisible({ timeout: 15_000 });
    const sw = page.locator("[data-kbc-rdiff-author-only]");
    await expect(sw).toBeVisible({ timeout: 15_000 });
    await expect(page.locator("[data-kbc-rdiff-author-count]")).toContainText("base moved");

    // Off: the upstream-only file is in the list beside the author's.
    await expect(page.locator(`[data-kbc-rdiff-file="${FILE_A}"]`)).toBeVisible({ timeout: 15_000 });
    await expect(page.locator(`[data-kbc-rdiff-file="${FILE_B}"]`)).toBeVisible();

    // On: only what the author touched.
    await sw.locator("input").check();
    await expect(page.locator(`[data-kbc-rdiff-file="${FILE_B}"]`)).toHaveCount(0, {
      timeout: 10_000,
    });
    await expect(page.locator(`[data-kbc-rdiff-file="${FILE_A}"]`)).toBeVisible();

    // Off again restores the full interdiff (the switch is a view, not state
    // anywhere on the daemon).
    await sw.locator("input").uncheck();
    await expect(page.locator(`[data-kbc-rdiff-file="${FILE_B}"]`)).toBeVisible({
      timeout: 10_000,
    });
  });
});
