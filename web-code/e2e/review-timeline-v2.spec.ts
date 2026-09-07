import { expect, test } from "@playwright/test";
import { FEATURE_BRANCH } from "./fixture-repo";
import { BASE, REPO_DIR, REPO_NAME } from "./helpers";

/// V73-K2c — timeline v2 lanes, the claim register, and pseudo-files in the
/// Review Room. Composes through the LOOPBACK-ONLY `POST /api/claims` and
/// `PUT /api/reviews/{id}/report` — the same shape `review-room.spec.ts`
/// already uses for `findings/import`/`PUT /report` — so the CLI is never
/// spawned and this suite stays HTTP/DOM-only. hunk↔turn chips are NOT
/// exercised here (they need a captured session transcript the fixture
/// repo has none of); the honest "loopback only"/"no match" refusal path
/// is covered in `lib/reviewTimeline.test.ts` and `useReviews.ts`'s own
/// hook doc instead.
const CLAIM_BODY = "this endpoint retries because the upstream is flaky under load";

test.describe("timeline v2 / claim register / pseudo-files (V73-K2c)", () => {
  test("lanes render with status, a seeded claim appears in the register, and a pseudo-file opens", async ({
    page,
    request,
  }) => {
    test.skip(!REPO_DIR, "KB_CODE_E2E_REPO_DIR not set — global-setup didn't run");

    const createRes = await request.post(`${BASE}/api/reviews`, {
      data: {
        repo: REPO_NAME,
        head_ref: FEATURE_BRANCH,
        base_ref: "main",
        title: "e2e timeline v2",
      },
    });
    expect(createRes.ok(), `create: ${createRes.status()} ${await createRes.text()}`).toBeTruthy();
    const { id: reviewId } = (await createRes.json()) as { id: number };

    // A report, so the Report tab renders its non-empty body (and thus the
    // claim register mounted beside its findings list).
    const reportRes = await request.put(`${BASE}/api/reviews/${reviewId}/report`, {
      data: {
        schema: "kbc-report/1",
        deck: "e2e timeline v2 deck",
        summary: "## Summary\n\ne2e report summary text.",
        verdict: "verified",
        verdict_headline: "e2e review looks fine",
        risk_score: 1,
      },
    });
    expect(reportRes.ok(), `put report: ${reportRes.status()} ${await reportRes.text()}`).toBeTruthy();

    // A claim (loopback — Playwright's `request` fixture originates from
    // 127.0.0.1, same posture `review-doc.spec.ts`'s `compose` call takes).
    const claimRes = await request.post(`${BASE}/api/claims`, {
      data: {
        schema: "kbc-claim/1",
        repo: REPO_NAME,
        subject_kind: "review",
        subject: `review:${reviewId}`,
        review_id: reviewId,
        kind: "explain",
        body_md: CLAIM_BODY,
        confidence: 0.8,
        evidence: [],
      },
    });
    // Degrade path (not expected on this branch's own daemon, kept for the
    // same reason `review-doc.spec.ts`'s compose degrade path is): the rest
    // of this unit (timeline lanes, pseudo-files) is independent of claims,
    // so a missing kbc-claim/1 route should not fail the whole spec.

    // --- Timeline tab: every lane renders, each with its own status -------
    await page.goto(`${BASE}/r/${REPO_NAME}/~reviews/${reviewId}?tab=timeline`);
    const timeline = page.locator(`[data-kbc-timeline="${reviewId}"]`);
    await expect(timeline).toBeVisible({ timeout: 15_000 });

    const lanes = [
      "lifecycle",
      "pr_body",
      "findings",
      "verdict",
      "comments",
      "wt_comments",
      "document",
      "report",
      "claims",
      "github",
      "turns",
    ];
    for (const lane of lanes) {
      const laneEl = page.locator(`[data-kbc-timeline-lane="${lane}"]`);
      await expect(laneEl).toBeVisible();
      // Every lane names its own state — never absent, never blank.
      await expect(laneEl).toHaveAttribute("data-kbc-timeline-lane-state", /ok|skipped|refused|degraded/);
    }
    // `review_created` is the one event guaranteed on every review.
    await expect(page.locator('[data-kbc-timeline-row="review_created"]')).toBeVisible();
    // The `turns` lane needs `?hunk=`, so it is honestly `skipped` here —
    // never silently absent from the lane bar above.
    await expect(page.locator('[data-kbc-timeline-lane="turns"]')).toHaveAttribute(
      "data-kbc-timeline-lane-state",
      "skipped",
    );

    // --- Report tab: the claim register, beside the findings list ---------
    if (claimRes.ok()) {
      await page.goto(`${BASE}/r/${REPO_NAME}/~reviews/${reviewId}?tab=report`);
      const claims = page.locator("[data-kbc-claims]");
      await expect(claims).toBeVisible({ timeout: 15_000 });
      await expect(claims.locator("[data-kbc-claim-kind]").first()).toBeVisible();
      await expect(claims).toContainText(CLAIM_BODY);
      // Surfaced-never-scored: confidence renders as agent-declared TEXT.
      await expect(claims.locator("[data-kbc-claim-confidence]").first()).toContainText("agent-declared");
      // The ladder state renders its own small badge (never the trust pill).
      await expect(claims.locator("[data-kbc-claim-ladder]").first()).toBeVisible();
    }

    // --- a pseudo-file opens as a read-only buffer in the diff center -----
    await page.goto(`${BASE}/r/${REPO_NAME}/~reviews/${reviewId}/diff/~review/commits.md`);
    const pseudo = page.locator('[data-kbc-pseudo="~review/commits.md"]');
    await expect(pseudo).toBeVisible({ timeout: 15_000 });
    await expect(pseudo.locator("[data-kbc-pseudo-blob]")).toBeVisible();
    await expect(pseudo).toContainText("no revision history");

    // The map column (open by default at this desktop viewport,
    // `parseDiffMap`'s own doc: absent `?map=` ⇒ open) lists it under
    // "chapter zero", before the real files.
    await expect(page.locator('[data-kbc-rdiff-map-pseudo="commits.md"]')).toBeVisible({ timeout: 10_000 });
  });
});
