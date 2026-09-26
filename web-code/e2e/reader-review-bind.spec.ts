import { expect, test } from "@playwright/test";
import { FEATURE_BRANCH, KNOWN_FILE, KNOWN_SYMBOL } from "./fixture-repo";
import { BASE, REPO_NAME } from "./helpers";

/// V80-M2 — "the reader binds a comment to a review." M0 shipped
/// `PUT`/`DELETE /api/annotations/{id}/review` + `in_diff` on
/// `GET /api/reviews/{id}/comments` groups; M3 shipped the browser-only
/// "current review" marker + the reader's `?review=` mirror. This unit
/// wires the two together: the composer's "Review" selector
/// (`ReviewBindSelector.tsx`, shared by `AnnotationsPanel`/
/// `DiffLineComposer`), each card's binding chip + bind/rebind/unbind
/// actions, and the rail's real Review tab (`ReviewFileThreadsPanel.tsx`).
///
/// Flow (matches the brief): open the fixture review's Room (sets the
/// current review) → reader on `KNOWN_FILE` (NOT one of `feature-x`'s own
/// changed files — `feature-x` only adds `feature_x.rs`, see
/// `fixture-repo.ts`) → `a` → compose with the review preselected → the
/// hint honestly says this file isn't in the diff → save → the server's
/// own `GET /api/reviews/{id}/comments` lists it under a group with
/// `in_diff: false` → the rail's Review tab lists the same thread → the
/// card's "Unbind" removes it from the review (still a plain working-tree
/// annotation) → "Bind to review…" puts it back.

interface ReviewCommentsBody {
  schema: string;
  review_id: number;
  repo: string;
  ps: number;
  groups: Array<{
    path: string;
    in_diff: boolean;
    comments: Array<{ id: string; body: string }>;
  }>;
}

interface AnnotationsBody {
  repo: string;
  path: string;
  annotations: Array<{ id: string; body: string; review_id?: number }>;
}

async function fetchReviewComments(
  request: import("@playwright/test").APIRequestContext,
  reviewId: number,
): Promise<ReviewCommentsBody> {
  const res = await request.get(`${BASE}/api/reviews/${reviewId}/comments`);
  expect(res.ok(), `GET comments: ${res.status()} ${await res.text()}`).toBeTruthy();
  return (await res.json()) as ReviewCommentsBody;
}

async function fetchAnnotations(
  request: import("@playwright/test").APIRequestContext,
  path: string,
): Promise<AnnotationsBody> {
  const res = await request.get(`${BASE}/api/annotations?repo=${REPO_NAME}&path=${encodeURIComponent(path)}`);
  expect(res.ok(), `GET annotations: ${res.status()} ${await res.text()}`).toBeTruthy();
  return (await res.json()) as AnnotationsBody;
}

test.describe("the reader binds a comment to a review (V80-M2)", () => {
  test("compose preselected, in_diff:false, unbind, rebind", async ({ page, request }) => {
    const createRes = await request.post(`${BASE}/api/reviews`, {
      data: {
        repo: REPO_NAME,
        head_ref: FEATURE_BRANCH,
        base_ref: "main",
        title: "e2e reader review-bind",
      },
    });
    expect(createRes.ok(), `create review: ${createRes.status()} ${await createRes.text()}`).toBeTruthy();
    const created = (await createRes.json()) as { id: number };
    const reviewId = created.id;

    // --- Room visit sets the current-review marker (M3) -------------------
    await page.goto(`${BASE}/r/${REPO_NAME}/~reviews/${reviewId}`);
    await expect(page.locator("[data-kbc-review-title]")).toContainText("e2e reader review-bind");

    // --- reader on a file NOT in feature-x's own diff ----------------------
    await page.goto(`${BASE}/r/${REPO_NAME}/${KNOWN_FILE}`);
    await expect(page.locator(".kbc-codeview")).toContainText(KNOWN_SYMBOL, { timeout: 10_000 });
    await page.locator(".kbc-codeview .cm-line").first().click();
    await page.keyboard.press("a");

    // The composer's OWN selector (scoped — a card's inline bind picker,
    // opened later in this test, carries the identical attribute).
    const composerSelect = page.locator(".kbc-annotations__composer [data-kbc-annot-review-select]");
    await expect(composerSelect).toBeVisible({ timeout: 10_000 });
    await expect(composerSelect).toHaveValue(String(reviewId), { timeout: 10_000 });
    await expect(page.locator(".kbc-annotations__composer [data-kbc-annot-review-hint]")).toContainText(
      "not in this review's diff",
      { timeout: 10_000 },
    );

    const commentBody = "M2 reader-review-bind e2e comment";
    await page.locator("[data-kbc-annot-body-input]").fill(commentBody);
    await page.locator("[data-kbc-annot-save]").click();

    const thread = page.locator(".kbc-annotations__thread", { hasText: commentBody });
    await expect(thread).toBeVisible({ timeout: 10_000 });
    await expect(thread.locator("[data-kbc-annot-review-chip]")).toBeVisible();

    // --- server truth: a group for KNOWN_FILE, in_diff:false, our comment -
    let comments = await fetchReviewComments(request, reviewId);
    let group = comments.groups.find((g) => g.path === KNOWN_FILE);
    expect(group, `expected a comments group for ${KNOWN_FILE}`).toBeTruthy();
    expect(group!.in_diff).toBe(false);
    expect(group!.comments.some((c) => c.body === commentBody)).toBe(true);

    // --- the rail's Review tab lists the same thread -----------------------
    await page.locator('[data-kbc-itab="review"]').click();
    await expect(page.locator("[data-kbc-review-rail-caption]")).toContainText("Not in the diff", {
      timeout: 10_000,
    });
    await expect(
      page.locator("[data-kbc-review-rail-thread]", { hasText: commentBody }),
    ).toBeVisible({ timeout: 10_000 });

    // --- back to Notes: unbind — still a working-tree note -----------------
    await page.locator('[data-kbc-itab="notes"]').click();
    await expect(thread).toBeVisible();
    await thread.locator("[data-kbc-annot-review-unbind]").click();
    await expect(thread.locator("[data-kbc-annot-review-chip]")).toHaveCount(0, { timeout: 10_000 });
    await expect(thread.locator("[data-kbc-annot-review-toggle]")).toHaveText("Bind to review…");

    comments = await fetchReviewComments(request, reviewId);
    group = comments.groups.find((g) => g.path === KNOWN_FILE);
    expect(group?.comments.some((c) => c.body === commentBody) ?? false).toBe(false);

    const annotations = await fetchAnnotations(request, KNOWN_FILE);
    const plain = annotations.annotations.find((a) => a.body === commentBody);
    expect(plain, "unbind must leave the annotation itself in place").toBeTruthy();
    expect(plain!.review_id).toBeUndefined();

    // --- "bind to review…" puts it back -------------------------------------
    await thread.locator("[data-kbc-annot-review-toggle]").click();
    const picker = thread.locator("[data-kbc-annot-review-picker]");
    await expect(picker).toBeVisible();
    const pickerSelect = picker.locator("[data-kbc-annot-review-select]");
    await pickerSelect.selectOption(String(reviewId));
    await picker.locator("[data-kbc-annot-review-apply]").click();
    await expect(picker).toHaveCount(0, { timeout: 10_000 });
    await expect(thread.locator("[data-kbc-annot-review-chip]")).toBeVisible({ timeout: 10_000 });

    comments = await fetchReviewComments(request, reviewId);
    group = comments.groups.find((g) => g.path === KNOWN_FILE);
    expect(group?.comments.some((c) => c.body === commentBody)).toBe(true);
  });
});
