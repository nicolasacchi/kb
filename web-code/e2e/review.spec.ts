import { execFileSync } from "node:child_process";
import { writeFileSync } from "node:fs";
import { join } from "node:path";
import { expect, test } from "@playwright/test";
import { FEATURE_BRANCH, KNOWN_FILE, KNOWN_SYMBOL, TEXT_NEEDLE } from "./fixture-repo";
import { BASE, REPO_DIR, REPO_NAME } from "./helpers";

/// Phase G-server ("kb-code v2 — The Operable Reader," the review-workflow
/// endpoints) end to end: the Compare page's merge-readiness card + the
/// session-grouped review toggle (G1/G3), the repo-state banner over a REAL
/// mid-merge conflict (G2), the range-diff page over a REAL rebase-amend
/// (C6), and the PR overlay's honest non-GitHub-origin empty state (G4 —
/// `POST /api/prs/fetch`/the PR list itself need a real GitHub origin this
/// fixture deliberately has none of, so only the "not a github origin"
/// path is exercised here).
///
/// Git-state choreography: two of these specs (`repo-state banner`,
/// `range-diff page`) mutate the SHARED fixture repo directly on disk
/// (`REPO_DIR` — same direct-fs pattern `checkout-dirty.spec.ts` uses,
/// extended to real git commands via `execFileSync` rather than a bare
/// `writeFileSync`). `main`'s own tip/content is load-bearing for
/// `story.spec.ts`/`time.spec.ts` (run later, alphabetically after this
/// file), so EVERY throwaway branch this file creates is built OFF `main`
/// (never moving `main`'s own ref) and every test's own cleanup — run both
/// inline (`try`/`finally`) AND again in `test.afterAll` as a backstop —
/// aborts any in-flight merge, checks out `main`, and deletes the
/// throwaway branches. Both cleanup call sites are idempotent (every git
/// command is wrapped in its own best-effort `try`/`catch`), so running
/// twice is harmless.

function git(args: string[]): string {
  return execFileSync("git", ["-C", REPO_DIR, ...args], { encoding: "utf-8" }).trim();
}

/// Best-effort — never throws, so a cleanup call is safe to run from
/// BOTH the test body's own `finally` and `test.afterAll` without one
/// throwing over the other.
function tryGit(args: string[]): void {
  try {
    execFileSync("git", ["-C", REPO_DIR, ...args], { stdio: "ignore" });
  } catch {
    // best-effort cleanup — see this file's own module doc.
  }
}

/// `KNOWN_FILE`'s fixture content (see `fixture-repo.ts`) with its one
/// arithmetic line swapped out — the two conflict branches below each pass
/// a DIFFERENT `thirdLine`, so a merge between them conflicts on exactly
/// that line and nothing else.
function knownFileContent(thirdLine: string): string {
  return [
    `fn ${KNOWN_SYMBOL}(a: i32, b: i32) -> i32 {`,
    `    // ${TEXT_NEEDLE}`,
    thirdLine,
    "}",
    "",
    "fn helper() -> i32 {",
    `    ${KNOWN_SYMBOL}(1, 2)`,
    "}",
    "",
  ].join("\n");
}

test.describe("merge-check card (Compare, G1)", () => {
  test("main -> feature-x shows clean + ahead/behind", async ({ page }) => {
    test.skip(!REPO_DIR, "KB_CODE_E2E_REPO_DIR not set — global-setup didn't run");

    await page.goto(`${BASE}/r/${REPO_NAME}/~compare?from=main&to=${FEATURE_BRANCH}`);

    const card = page.locator("[data-kbc-mergecheck]");
    await expect(card).toBeVisible();
    await expect(card.locator("[data-kbc-mergecheck-clean]")).toBeVisible();
    await expect(card.locator("[data-kbc-mergecheck-ahead]")).toContainText("1");
    // 8 = main's fixture commits since the feature-x fork point (see
    // time.spec's branches test for the enumeration, incl. DCB W2.B's
    // additive rev_remap demo commits, DCB-W2.B.R fix 9's ambiguity-demo
    // commit, and DCB-W3.B's seedCitedByDemo commit ("doclens: add cited-by
    // demo fixture")).
    await expect(card.locator("[data-kbc-mergecheck-behind]")).toContainText("8");
    await expect(card.locator("[data-kbc-mergecheck-mergebase]")).toBeVisible();
  });
});

test.describe("repo-state banner — a real mid-merge conflict (G2)", () => {
  const BRANCH_A = "review-conflict-a";
  const BRANCH_B = "review-conflict-b";

  function restore() {
    if (!REPO_DIR) return;
    tryGit(["merge", "--abort"]);
    tryGit(["checkout", "-q", "main"]);
    tryGit(["branch", "-D", BRANCH_A]);
    tryGit(["branch", "-D", BRANCH_B]);
  }

  test.afterAll(restore);

  test("mid-merge banner shows the conflicted chip; the reader tints the marker lines", async ({ page }) => {
    test.skip(!REPO_DIR, "KB_CODE_E2E_REPO_DIR not set — global-setup didn't run");

    try {
      // Two THROWAWAY branches, both forked from `main`'s own tip (never
      // moving `main` itself), each editing lib.rs's one arithmetic line
      // differently — merging one into the other conflicts on exactly
      // that line.
      git(["checkout", "-q", "-b", BRANCH_A, "main"]);
      writeFileSync(join(REPO_DIR, KNOWN_FILE), knownFileContent("    a + b + 100 // conflict-a"));
      git(["add", "-A"]);
      git(["commit", "-q", "-m", "conflict-a edits lib.rs"]);

      git(["checkout", "-q", "-b", BRANCH_B, "main"]);
      writeFileSync(join(REPO_DIR, KNOWN_FILE), knownFileContent("    a + b + 200 // conflict-b"));
      git(["add", "-A"]);
      git(["commit", "-q", "-m", "conflict-b edits lib.rs"]);

      // The working tree is now parked on `review-conflict-b` — a
      // throwaway ref, NOT `main` — for the rest of this test, restored by
      // `restore()` above.
      let mergeThrew = false;
      try {
        execFileSync("git", ["-C", REPO_DIR, "merge", BRANCH_A], { stdio: "ignore" });
      } catch {
        mergeThrew = true;
      }
      expect(mergeThrew, "the merge must actually conflict for this test to mean anything").toBe(true);

      await page.goto(`${BASE}/r/${REPO_NAME}`);
      const banner = page.locator("[data-kbc-repostate-banner]");
      await expect(banner).toBeVisible();
      await expect(banner).toHaveAttribute("data-kbc-repostate-op", "merge");
      const chip = banner.locator(`[data-kbc-repostate-conflict="${KNOWN_FILE}"]`);
      await expect(chip).toBeVisible();

      // Follow the conflict chip into the reader — it opens the working
      // tree (no `?ref=`), which the daemon serves as-is: raw conflict
      // markers, no parse required. Phase G2's CM6 overlay tints every
      // marker line.
      await chip.click();
      await expect(page.locator(".kbc-codeview")).toBeVisible();
      const conflictLines = page.locator(".kbc-conflict-line");
      await expect(conflictLines).not.toHaveCount(0);
    } finally {
      restore();
    }
  });
});

test.describe("range-diff page (C6)", () => {
  const V1 = "review-rd-v1";
  const V2 = "review-rd-v2";

  function restore() {
    if (!REPO_DIR) return;
    tryGit(["checkout", "-q", "main"]);
    tryGit(["branch", "-D", V1]);
    tryGit(["branch", "-D", V2]);
  }

  test.afterAll(restore);

  test("renders >= 1 modified pair from a rebase-amend built on a throwaway branch", async ({ page }) => {
    test.skip(!REPO_DIR, "KB_CODE_E2E_REPO_DIR not set — global-setup didn't run");

    try {
      const baseSha = git(["rev-parse", "main"]);

      git(["checkout", "-q", "-b", V1, "main"]);
      writeFileSync(join(REPO_DIR, "review_rd.rs"), "fn review_rd() -> i32 {\n    1\n}\n");
      git(["add", "-A"]);
      git(["commit", "-q", "-m", "add review_rd.rs"]);

      // v2 = v1's own commit, amended (same tree-shaping intent, different
      // sha/message) — a real rebase-amend fixture, mirroring
      // `history::range_diff`'s own Rust test.
      git(["checkout", "-q", "-b", V2, V1]);
      git(["commit", "--amend", "-q", "-m", "add review_rd.rs (amended)"]);

      const oldRange = `${baseSha}..${V1}`;
      const newRange = `${baseSha}..${V2}`;
      await page.goto(
        `${BASE}/r/${REPO_NAME}/~range-diff?old=${encodeURIComponent(oldRange)}&new=${encodeURIComponent(newRange)}`,
      );

      const modifiedRows = page.locator('[data-kbc-rangediff-pair="modified"]');
      await expect(modifiedRows).toHaveCount(1);
      await expect(modifiedRows.locator(".kbc-rangediff__subject")).toHaveText("add review_rd.rs");
    } finally {
      restore();
    }
  });
});

test.describe("PR overlay — non-GitHub origin (G4)", () => {
  test("~prs shows the not-a-GitHub-origin EmptyState", async ({ page }) => {
    // No REPO_DIR git mutation here — the fixture repo simply has no
    // `origin` remote configured at all (`fixture-repo.ts` never adds
    // one), which `github::github_repo` reports as cleanly as a
    // wrong-host origin (`routes.rs`'s `GithubError` → 400 mapping, "not a
    // github origin" either way).
    await page.goto(`${BASE}/r/${REPO_NAME}/~prs`);

    const empty = page.locator("[data-kbc-empty]");
    await expect(empty).toBeVisible();
    await expect(empty).toContainText(/not a github origin/i);
  });
});

test.describe("Compare grouped toggle (G3)", () => {
  test("fixture commits are unattributed -> the 'no recorded session' bucket renders all commits; flat toggle works", async ({
    page,
  }) => {
    // `from=feature-x, to=main` (two-dot): every commit reachable from
    // `main` but not `feature-x` — the two `story.rs` commits
    // (`fixture-repo.ts`'s own doc on why they moved main's HEAD past the
    // initial commit) — with kb_daemon disabled for this whole e2e run
    // (`global-setup.ts`), every one of them resolves to an honest
    // `confidence: "none"` miss, so 0 distinct attributed sessions ⇒ the
    // DEFAULT view is flat (`defaultGroupedView`'s own threshold).
    await page.goto(`${BASE}/r/${REPO_NAME}/~compare?from=${FEATURE_BRANCH}&to=main`);

    await expect(page.locator("[data-kbc-compare-groups]")).toHaveCount(0);
    await expect(page.locator('[data-kbc-compare-view="flat"]')).toHaveClass(/is-active/);

    const commitCount = await page.locator(".kbc-compare__commit").count();
    expect(commitCount).toBeGreaterThanOrEqual(2);

    await page.locator('[data-kbc-compare-view="grouped"]').click();
    const groups = page.locator("[data-kbc-compare-group]");
    await expect(groups).toHaveCount(1);
    await expect(groups.first()).toHaveAttribute("data-kbc-compare-group", "none");
    await expect(groups.first().locator("[data-kbc-compare-group-none]")).toHaveText("no recorded session");
    await expect(groups.first().locator(".kbc-compare__commit")).toHaveCount(commitCount);

    await page.locator('[data-kbc-compare-view="flat"]').click();
    await expect(page.locator("[data-kbc-compare-groups]")).toHaveCount(0);
    await expect(page.locator(".kbc-compare__commit")).toHaveCount(commitCount);
  });
});
