import { execFileSync } from "node:child_process";
import { writeFileSync } from "node:fs";
import { join } from "node:path";
import { expect, test } from "@playwright/test";
import { FEATURE_BRANCH, FEATURE_FILE } from "./fixture-repo";
import { BASE, REPO_DIR, REPO_NAME } from "./helpers";

/// V4.D3 — full-page review diff. Follows reviews.spec.ts: create the
/// review via the API, then drive the SPA. Adds a second file on a
/// disposable branch so `n` has a neighbour to land on. Does NOT run
/// against main's tip.

const SECOND_FILE = "e2e_rdiff_extra.rs";
const RDIFF_BRANCH = "e2e-rdiff-page";

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

test.describe("review full-page diff (V4.D3)", () => {
  test.afterAll(() => {
    tryGit(["checkout", "main"]);
    tryGit(["branch", "-D", RDIFF_BRANCH]);
  });

  test("cockpit open, order, keys, viewed, deep-link, focus, Esc", async ({ page, request }) => {
    test.skip(!REPO_DIR, "KB_CODE_E2E_REPO_DIR not set — global-setup didn't run");

    tryGit(["checkout", "main"]);
    tryGit(["branch", "-D", RDIFF_BRANCH]);
    git(["checkout", "-q", "-b", RDIFF_BRANCH, FEATURE_BRANCH]);
    writeFileSync(join(REPO_DIR, SECOND_FILE), "fn e2e_rdiff_extra() -> i32 { 1 }\n");
    git(["add", SECOND_FILE]);
    git(["commit", "-q", "-m", "e2e rdiff second file"]);
    git(["checkout", "-q", "main"]);

    const createRes = await request.post(`${BASE}/api/reviews`, {
      data: {
        repo: REPO_NAME,
        head_ref: RDIFF_BRANCH,
        base_ref: "main",
        title: "e2e review diff page",
      },
    });
    expect(createRes.ok(), `create review: ${createRes.status()} ${await createRes.text()}`).toBeTruthy();
    const created = (await createRes.json()) as { id: number };
    const reviewId = created.id;

    await page.goto(`${BASE}/r/${REPO_NAME}/~reviews/${reviewId}`);
    await expect(page.locator("[data-kbc-review-title]")).toContainText("e2e review diff page");
    await expect(page.locator("[data-kbc-review-files]")).toBeVisible({ timeout: 10_000 });

    // Cockpit → "Open full page"
    await page.locator("[data-kbc-rdiff-open]").click();
    await expect(page).toHaveURL(new RegExp(`~reviews/${reviewId}/diff`));
    await expect(page.locator("[data-kbc-rdiff]")).toBeVisible({ timeout: 10_000 });

    const sections = page.locator("[data-kbc-rdiff-file]");
    await expect(sections).toHaveCount(2, { timeout: 10_000 });
    const firstPath = await sections.nth(0).getAttribute("data-kbc-rdiff-file");
    const secondPath = await sections.nth(1).getAttribute("data-kbc-rdiff-file");
    expect([firstPath, secondPath].sort()).toEqual([FEATURE_FILE, SECOND_FILE].sort());

    // `]f` advances the cursor file. V70-A5 retired the `n`/`p` pair here:
    // `n`/`N` is search-step in the reader, and `n`/`p` ALSO silently meant
    // "tour step" whenever a tour was running (recon R5). `[f`/`]f` was
    // already bound on this page and is the reader's own working-set pair, so
    // the two surfaces now agree. See `web-code/src/commands/MOVED.md`.
    const firstCurrent = page.locator(`[data-kbc-rdiff-file="${firstPath}"] [data-kbc-diff-current]`);
    await expect(firstCurrent.first()).toBeVisible({ timeout: 10_000 });
    await page.keyboard.press("]");
    await page.keyboard.press("f");
    await expect(
      page.locator(`[data-kbc-rdiff-file="${secondPath}"] [data-kbc-diff-current]`).first(),
    ).toBeVisible();

    // `j` advances hunks (1-hunk first file → crosses into the next).
    await page.keyboard.press("k"); // back to file 1 last/only hunk
    await expect(
      page.locator(`[data-kbc-rdiff-file="${firstPath}"] [data-kbc-diff-current]`).first(),
    ).toBeVisible();
    await page.keyboard.press("j");
    await expect(
      page.locator(`[data-kbc-rdiff-file="${secondPath}"] [data-kbc-diff-current]`).first(),
    ).toBeVisible();

    // `Space v` toggles viewed; toolbar progress updates (same attr as the
    // cockpit). MOVED under the leader in V70-A5 — bare `v` is visual select
    // in the reader, and D2 rules that a review-diff verb colliding with a
    // reader verb moves.
    const progress = page.locator("[data-kbc-review-progress]");
    const before = await progress.getAttribute("data-kbc-review-progress");
    await page.keyboard.press("Space");
    await page.keyboard.press("v");
    await expect(progress).not.toHaveAttribute("data-kbc-review-progress", before ?? "", {
      timeout: 10_000,
    });
    const after = await progress.getAttribute("data-kbc-review-progress");
    expect(after).toMatch(/^\d+\/\d+$/);

    // `?line=N&side=new` deep-link: scroll + flash.
    await page.goto(
      `${BASE}/r/${REPO_NAME}/~reviews/${reviewId}/diff?line=1&side=new&file=${encodeURIComponent(FEATURE_FILE)}`,
    );
    await expect(page.locator("[data-kbc-rdiff]")).toBeVisible({ timeout: 10_000 });
    await expect(page.locator(".kbc-rdiff__flash")).toBeVisible({ timeout: 10_000 });

    // Single-file focus URL: one section + prev/next footer.
    await page.goto(`${BASE}/r/${REPO_NAME}/~reviews/${reviewId}/diff/${FEATURE_FILE}`);
    await expect(page.locator("[data-kbc-rdiff]")).toHaveAttribute("data-kbc-rdiff-mode", "single");
    await expect(page.locator("[data-kbc-rdiff-file]")).toHaveCount(1);
    await expect(page.locator("[data-kbc-rdiff-footer]")).toBeVisible();
    await expect(page.locator("[data-kbc-rdiff-footer-prev], [data-kbc-rdiff-footer-next]").first()).toBeVisible();

    // `u` returns to the cockpit. V70-A5: Esc NEVER navigates (§P2's Esc
    // class) — it only dismisses, innermost first. Going back is `u`
    // (`nav.back`, i.e. `navigate(-1)` — ONE session-history step, same as
    // the browser's own Back) or the browser's own Back, both of which
    // restore the position you left, which the old Esc→cockpit jump threw
    // away. This flow pushed FOUR history entries (cockpit → the
    // `[data-kbc-rdiff-open]` Link → the `?line=` deep-link `goto` → the
    // single-file `goto`), so landing back on the cockpit is one
    // `page.goBack()` (single-file → deep-link) plus TWO `u` steps
    // (deep-link → multi-file diff → cockpit), not one. Each step waits for
    // its own URL to land before firing the next key: `navigate(-1)` is an
    // async client-side transition, and firing a second `u` while the first
    // is still in flight can get swallowed (observed empirically — two
    // presses back-to-back landed only ONE entry back, not two).
    //
    // V70-H1 — the final landing's `$`-anchored regex is a STALE
    // expectation, not an app bug: V70-A3S (already on `main`) moved the
    // cockpit's tab and patchset INTO the URL (`?tab=…`, `?ps=…`), so
    // returning to the cockpit now legitimately lands on
    // `~reviews/<id>?tab=…`, never just the bare path. `(\?|$)` accepts
    // either. The intermediate multi-file-diff landing gets the same
    // treatment defensively (its own `ps`/`view` params are optional but
    // not guaranteed absent).
    await page.goBack();
    await expect(page).toHaveURL(new RegExp(`~reviews/${reviewId}/diff\\?`));
    await page.keyboard.press("u");
    await expect(page).toHaveURL(new RegExp(`~reviews/${reviewId}/diff(\\?|$)`));
    await page.keyboard.press("u");
    await expect(page).toHaveURL(new RegExp(`~reviews/${reviewId}(\\?|$)`));
    await expect(page.locator("[data-kbc-review-title]")).toBeVisible();
  });

  // V70-A3S — PRR-U3's short finding permalink (`codeUrl.ts`'s
  // `findingUrl`) never had a route registered for it before this unit;
  // every `/f/<slug>` link 404'd through the Reader catch-all. The redirect
  // fires regardless of whether the slug resolves to a real finding —
  // resolution is `?finding=`'s own concern on the diff route, not this
  // ramp's.
  test("short finding permalink /f/<slug> redirects to the diff route's ?finding=", async ({
    page,
    request,
  }) => {
    test.skip(!REPO_DIR, "KB_CODE_E2E_REPO_DIR not set — global-setup didn't run");

    const createRes = await request.post(`${BASE}/api/reviews`, {
      data: {
        repo: REPO_NAME,
        head_ref: FEATURE_BRANCH,
        base_ref: "main",
        title: "e2e finding permalink",
      },
    });
    expect(createRes.ok(), `create review: ${createRes.status()} ${await createRes.text()}`).toBeTruthy();
    const created = (await createRes.json()) as { id: number };
    const reviewId = created.id;

    await page.goto(`${BASE}/r/${REPO_NAME}/~reviews/${reviewId}/f/f-does-not-exist`);
    await expect(page).toHaveURL(new RegExp(`~reviews/${reviewId}/diff\\?finding=f-does-not-exist`));
    await expect(page.locator("[data-kbc-rdiff]")).toBeVisible({ timeout: 10_000 });
  });
});
