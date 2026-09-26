import { execFileSync } from "node:child_process";
import { writeFileSync } from "node:fs";
import { join } from "node:path";
import { expect, test, type APIRequestContext } from "@playwright/test";
import { KNOWN_FILE } from "./fixture-repo";
import { BASE, REPO_DIR, REPO_NAME } from "./helpers";

/// V80-M4 — the milestone's three operator scenarios for "the Room reads
/// human threads first-class": (1) a comment on a file OUTSIDE the diff
/// (via `?files=all`, M1) shows up in the Room's "Outside the diff"
/// section with a deep link that lands on the right line; (2) a comment
/// created with the review already scoped is not `in_diff` and appears in
/// the Room; (3) binding an older working-tree note moves it into the Room
/// LIVE (SSE), and unbinding moves it back out — no reload either way.
///
/// M2 ("the reader's bind selector + rail Review tab," PR #126) is NOT on
/// this unit's base — `routes/Reader.tsx`'s own comment says so literally
/// ("Threads for this file arrive with M2"), so the plain reader has no UI
/// yet that attaches `review_id` to a note it composes. Scenarios 2 and 3
/// therefore drive that ONE step (create-with-`review_id`, and bind/unbind)
/// directly against the API — the exact wire effect M2's future UI
/// affordance will produce — rather than against a reader composer that
/// does not exist on this base. What THIS unit built, and what these three
/// tests actually prove, is the ROOM's live behaviour once that scope is
/// set: sectioning, counts, the reader deep link, and SSE responsiveness.
///
/// Disposable branch off `main` directly (never `feature-x`, so
/// `KNOWN_FILE` stays untouched — exactly the "outside the diff" fixture
/// every scenario below needs, the same precedent
/// `review-diff-anyfile.spec.ts` (V80-M1) establishes).

const MARGIN_BRANCH = "e2e-margin-scenarios";
const MARGIN_ADDED = "e2e_margin_scenarios.rs";

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

async function createReview(request: APIRequestContext, title: string): Promise<number> {
  const res = await request.post(`${BASE}/api/reviews`, {
    data: { repo: REPO_NAME, head_ref: MARGIN_BRANCH, base_ref: "main", title },
  });
  expect(res.ok(), `create review: ${res.status()} ${await res.text()}`).toBeTruthy();
  return ((await res.json()) as { id: number }).id;
}

async function commentGroups(
  request: APIRequestContext,
  reviewId: number,
): Promise<Array<{ path: string; in_diff: boolean; comments: unknown[] }>> {
  const res = await request.get(`${BASE}/api/reviews/${reviewId}/comments?all=true`);
  expect(res.ok(), `comments: ${res.status()} ${await res.text()}`).toBeTruthy();
  const body = (await res.json()) as { groups: Array<{ path: string; in_diff: boolean; comments: unknown[] }> };
  return body.groups;
}

test.describe("the Room reads human threads first-class (V80-M4)", () => {
  test.beforeAll(() => {
    if (!REPO_DIR) return;
    tryGit(["checkout", "main"]);
    tryGit(["branch", "-D", MARGIN_BRANCH]);
    git(["checkout", "-q", "-b", MARGIN_BRANCH, "main"]);
    writeFileSync(join(REPO_DIR, MARGIN_ADDED), "fn e2e_margin_scenarios() -> i32 { 1 }\n");
    git(["add", MARGIN_ADDED]);
    git(["commit", "-q", "-m", "e2e margin-scenarios fixture"]);
    git(["checkout", "-q", "main"]);
  });

  test.afterAll(() => {
    if (!REPO_DIR) return;
    tryGit(["checkout", "main"]);
    tryGit(["branch", "-D", MARGIN_BRANCH]);
  });

  test("scenario 1 — ?files=all, comment on an unchanged file, lands in Outside the diff with a working reader deep link", async ({
    page,
    request,
  }) => {
    test.skip(!REPO_DIR, "KB_CODE_E2E_REPO_DIR not set — global-setup didn't run");
    const reviewId = await createReview(request, "e2e margin scenario 1");

    // --- ?files=all → open KNOWN_FILE (unchanged) → comment on line 1 ---
    await page.goto(`${BASE}/r/${REPO_NAME}/~reviews/${reviewId}/diff?files=all`);
    await expect(page.locator("[data-kbc-rdiff]")).toBeVisible({ timeout: 15_000 });
    const unchangedRow = page.locator(`[data-kbc-rdiff-map-row="${KNOWN_FILE}"]`);
    await expect(unchangedRow).toBeVisible({ timeout: 10_000 });
    await expect(unchangedRow).toHaveAttribute("data-kbc-ftree-changed", "0");
    await unchangedRow.click();

    const section = page.locator(`[data-kbc-rdiff-file="${KNOWN_FILE}"]`);
    await expect(section).toBeVisible({ timeout: 10_000 });
    const gutterLine1 = section.locator('[data-kbc-review-compose-new="1"]');
    await expect(gutterLine1).toBeVisible({ timeout: 10_000 });
    await gutterLine1.click();
    const composer = section.locator("[data-kbc-review-composer-body]").first();
    await composer.fill("scenario 1 — a comment outside the diff");
    await section.locator("[data-kbc-review-composer-submit]").first().click();
    await expect(page.locator("[data-kbc-rdiff-drafts-count]")).toHaveText("1", { timeout: 10_000 });
    const batchReq = page.waitForRequest(
      (r) => r.url().includes("/api/annotations/batch") && r.method() === "POST",
    );
    await page.locator("[data-kbc-rdiff-drafts-publish]").click();
    await batchReq;

    // Server truth first (the same `in_diff: false` caption the Room reads).
    await expect
      .poll(
        async () => {
          const groups = await commentGroups(request, reviewId);
          const g = groups.find((row) => row.path === KNOWN_FILE);
          return g && g.in_diff === false ? g.comments.length : -1;
        },
        { timeout: 15_000 },
      )
      .toBe(1);

    // --- the Room: sectioned "Outside the diff", counts line, reader link ---
    await page.goto(`${BASE}/r/${REPO_NAME}/~reviews/${reviewId}`);
    const outsideSection = page.locator('[data-kbc-review-threads-section="outside-diff"]');
    await expect(outsideSection).toBeVisible({ timeout: 15_000 });
    await expect(outsideSection.locator(`[data-kbc-review-ann-path="${KNOWN_FILE}"]`)).toBeVisible();
    // The section-count line ALWAYS names all three, even the zero ones —
    // this review's own diff only touches `MARGIN_ADDED` (never commented
    // on), so "in the diff" and "general" both stay honestly at zero.
    await expect(page.locator("[data-kbc-review-threads-section-counts]")).toHaveText(
      "0 in the diff · 1 outside · 0 general",
    );
    await expect(page.locator('[data-kbc-review-threads-section="in-diff"]')).toHaveCount(0);
    await expect(page.locator('[data-kbc-review-threads-section="general"]')).toHaveCount(0);

    const readerLink = outsideSection.locator("[data-kbc-review-threads-open-reader]").first();
    await expect(readerLink).toBeVisible();
    const readerHref = await readerLink.getAttribute("href");
    expect(readerHref).toMatch(new RegExp(`^/r/${REPO_NAME}/${KNOWN_FILE}\\?ref=[0-9a-f]+&line=1&review=${reviewId}$`));
    await readerLink.click();
    await expect(page).toHaveURL(new RegExp(`/r/${REPO_NAME}/${KNOWN_FILE}\\?ref=.*line=1.*review=${reviewId}`));
  });

  test("scenario 2 — a comment created with the review already scoped is not in_diff, and the Room shows it", async ({
    request,
    page,
  }) => {
    test.skip(!REPO_DIR, "KB_CODE_E2E_REPO_DIR not set — global-setup didn't run");
    const reviewId = await createReview(request, "e2e margin scenario 2");

    // "reader with the current review set → comment on any file": M2's
    // future UI would attach `review_id` at create time (this file's own
    // module doc). Same wire shape, driven directly — `CreateAnnotationBody
    // .review_id`, the ORIGINAL create-time review scoping M0's own doc
    // names as pre-dating the bind route.
    const createRes = await request.post(`${BASE}/api/annotations`, {
      data: {
        repo: REPO_NAME,
        path: KNOWN_FILE,
        line: 1,
        anchor_kind: "line",
        side: "new",
        intent: "question",
        body: "scenario 2 — a review-scoped question on an unchanged file",
        author: "you",
        review_id: reviewId,
      },
    });
    expect(createRes.ok(), `create: ${createRes.status()} ${await createRes.text()}`).toBeTruthy();

    const groups = await commentGroups(request, reviewId);
    const g = groups.find((row) => row.path === KNOWN_FILE);
    expect(g?.in_diff).toBe(false);
    expect(g?.comments).toHaveLength(1);

    await page.goto(`${BASE}/r/${REPO_NAME}/~reviews/${reviewId}`);
    const outsideSection = page.locator('[data-kbc-review-threads-section="outside-diff"]');
    await expect(outsideSection).toBeVisible({ timeout: 15_000 });
    await expect(outsideSection.locator(`[data-kbc-review-ann-path="${KNOWN_FILE}"]`)).toBeVisible();
  });

  test("scenario 3 — binding an older working-tree note moves it into the Room live (SSE); unbind moves it out", async ({
    request,
    page,
  }) => {
    test.skip(!REPO_DIR, "KB_CODE_E2E_REPO_DIR not set — global-setup didn't run");
    const reviewId = await createReview(request, "e2e margin scenario 3");

    // An OLDER, plain working-tree note — no review scope at all, exactly
    // like a note a human wrote before this review ever existed.
    const createRes = await request.post(`${BASE}/api/annotations`, {
      data: {
        repo: REPO_NAME,
        path: KNOWN_FILE,
        line: 1,
        anchor_kind: "line",
        side: "new",
        intent: "note",
        body: "scenario 3 — an older working-tree note",
        author: "you",
      },
    });
    expect(createRes.ok(), `create: ${createRes.status()} ${await createRes.text()}`).toBeTruthy();
    const noteId = ((await createRes.json()) as { id: string }).id;

    // Open the Room FIRST — its SSE connection is live BEFORE the bind
    // below, so a live update (never a reload) is the only way the note
    // can appear.
    await page.goto(`${BASE}/r/${REPO_NAME}/~reviews/${reviewId}`);
    await expect(page.locator("[data-kbc-review-threads-section-counts]")).toHaveText(
      "0 in the diff · 0 outside · 0 general",
    );
    await expect(page.locator('[data-kbc-review-threads-section="outside-diff"]')).toHaveCount(0);

    const bindRes = await request.put(`${BASE}/api/annotations/${noteId}/review`, {
      data: { review_id: reviewId },
    });
    expect(bindRes.ok(), `bind: ${bindRes.status()} ${await bindRes.text()}`).toBeTruthy();

    await expect(page.locator(`[data-kbc-review-threads-row="${noteId}"]`)).toBeVisible({ timeout: 15_000 });
    await expect(page.locator('[data-kbc-review-threads-section="outside-diff"]')).toBeVisible();
    await expect(page.locator("[data-kbc-review-threads-section-counts]")).toHaveText(
      "0 in the diff · 1 outside · 0 general",
    );

    const unbindRes = await request.delete(`${BASE}/api/annotations/${noteId}/review`);
    expect(unbindRes.ok(), `unbind: ${unbindRes.status()} ${await unbindRes.text()}`).toBeTruthy();

    await expect(page.locator(`[data-kbc-review-threads-row="${noteId}"]`)).toHaveCount(0, { timeout: 15_000 });
    await expect(page.locator("[data-kbc-review-threads-section-counts]")).toHaveText(
      "0 in the diff · 0 outside · 0 general",
    );
  });
});
