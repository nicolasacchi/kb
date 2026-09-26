import { expect, test } from "@playwright/test";
import { FEATURE_BRANCH, FEATURE_FILE } from "./fixture-repo";
import { BASE, REPO_DIR, REPO_NAME } from "./helpers";

/// V80-M5 (D6) — "promote a bound comment to a finding": the human's OWN
/// review comment (a top-level, review-bound annotation, M0/M2's bind
/// surface) is a PEER of the agent's imported findings — see
/// `review_findings.rs`'s "Adoption" doc for the server contract this e2e
/// exercises end to end: create a review, comment on a line (loopback
/// `POST /api/annotations`, the same bind shape `annotate_bind.rs`'s
/// server tests use), open the Room, click "Promote to finding", submit
/// the small form, and assert BOTH the SPA (the thread now renders as a
/// finding card with a `you` author chip) and the wire (`GET
/// .../findings` lists it with `origin: "manual"` and the SAME
/// `annotation_id` the comment always had).
test.describe("promote a bound comment to a finding (V80-M5)", () => {
  test("end to end", async ({ page, request }) => {
    test.skip(!REPO_DIR, "KB_CODE_E2E_REPO_DIR not set — global-setup didn't run");

    // --- create the review --------------------------------------------------
    const createRes = await request.post(`${BASE}/api/reviews`, {
      data: {
        repo: REPO_NAME,
        head_ref: FEATURE_BRANCH,
        base_ref: "main",
        title: "e2e promote to finding",
      },
    });
    expect(createRes.ok(), `create review: ${createRes.status()} ${await createRes.text()}`).toBeTruthy();
    const created = (await createRes.json()) as { id: number };
    const reviewId = created.id;

    // --- a top-level, review-bound human comment (the promotion target) ----
    const commentRes = await request.post(`${BASE}/api/annotations`, {
      data: {
        repo: REPO_NAME,
        path: FEATURE_FILE,
        line: 2,
        body: "e2e: this line deserves a finding",
        review_id: reviewId,
        side: "new",
      },
    });
    expect(commentRes.ok(), `comment: ${commentRes.status()} ${await commentRes.text()}`).toBeTruthy();
    const comment = (await commentRes.json()) as { id: string };
    const annId = comment.id;

    // --- a report, so the Report tab's hero (and its "authored by you" chip)
    // renders instead of the "no agent review imported yet" empty state —
    // same minimal shape `review-room.spec.ts` PUTs.
    const reportRes = await request.put(`${BASE}/api/reviews/${reviewId}/report`, {
      data: {
        schema: "kbc-report/1",
        summary: "## Summary\n\ne2e promote-to-finding report summary.",
      },
    });
    expect(reportRes.ok(), `put report: ${reportRes.status()} ${await reportRes.text()}`).toBeTruthy();

    // --- open the Room, promote the comment ---------------------------------
    await page.goto(`${BASE}/r/${REPO_NAME}/~reviews/${reviewId}`);
    await expect(page.locator("[data-kbc-review-title]")).toContainText("e2e promote to finding");

    const toggle = page.locator(`[data-kbc-promote-finding-toggle="${annId}"]`);
    await expect(toggle).toBeVisible({ timeout: 10_000 });
    await expect(toggle).toBeEnabled();
    await toggle.click();

    const form = page.locator(`[data-kbc-promote-finding-form="${annId}"]`);
    await expect(form).toBeVisible();
    await form.locator("[data-kbc-promote-finding-severity]").selectOption("blocker");
    await form.locator("[data-kbc-promote-finding-title]").fill("e2e promoted finding title");
    await form.locator(`[data-kbc-promote-finding-submit="${annId}"]`).click();

    // --- the thread re-renders as a finding card, in place -----------------
    // `FindingRow` (the compact row `ReviewThreadsCard` swaps in) carries no
    // author attribute of its own (that's `FindingCard`'s full-card-only
    // `data-kbc-finding-author`) — its slug is the honest per-row identity.
    const findingRow = page.locator(`[data-kbc-finding-row-wrap]`);
    await expect(findingRow.first()).toBeVisible({ timeout: 10_000 });
    // The promote form/toggle is gone — the row that used to hold it now
    // renders the finding card instead (never BOTH at once).
    await expect(page.locator(`[data-kbc-promote-finding-toggle="${annId}"]`)).toHaveCount(0);

    // --- the Report tab's "authored by you" count reflects the promotion ---
    const reportPanel = page.locator(`[data-kbc-report-panel="${reviewId}"]`);
    await expect(reportPanel.locator("[data-kbc-room-hero-manual]")).toContainText("authored by you: 1", {
      timeout: 10_000,
    });

    // --- the wire agrees: a manual finding, same annotation id -------------
    const findingsRes = await request.get(`${BASE}/api/reviews/${reviewId}/findings`);
    expect(findingsRes.ok()).toBeTruthy();
    const findingsBody = (await findingsRes.json()) as {
      findings: { annotation_id: string; origin: string; title: string }[];
    };
    const promoted = findingsBody.findings.find((f) => f.annotation_id === annId);
    expect(promoted, JSON.stringify(findingsBody)).toBeTruthy();
    expect(promoted?.origin).toBe("manual");
    expect(promoted?.title).toBe("e2e promoted finding title");

    // --- promoting the SAME comment again is refused (409), never a second
    // finding — the reader-rail/Room UI reflects it as already-a-finding,
    // so this asserts the WIRE guard directly.
    const secondRes = await request.post(`${BASE}/api/reviews/${reviewId}/findings`, {
      data: { from_annotation_id: annId, severity: "concern", slug: "f-e2e-second-attempt" },
    });
    expect(secondRes.status()).toBe(409);
  });

  test("a general (path-less) comment refuses proactively, never a submit", async ({ page, request }) => {
    test.skip(!REPO_DIR, "KB_CODE_E2E_REPO_DIR not set — global-setup didn't run");

    const createRes = await request.post(`${BASE}/api/reviews`, {
      data: {
        repo: REPO_NAME,
        head_ref: FEATURE_BRANCH,
        base_ref: "main",
        title: "e2e promote general refusal",
      },
    });
    expect(createRes.ok()).toBeTruthy();
    const created = (await createRes.json()) as { id: number };
    const reviewId = created.id;

    const generalRes = await request.post(`${BASE}/api/annotations`, {
      data: {
        repo: REPO_NAME,
        path: "",
        anchor_kind: "review",
        body: "e2e: a general question, not a line",
        review_id: reviewId,
      },
    });
    expect(generalRes.ok(), `general comment: ${generalRes.status()} ${await generalRes.text()}`).toBeTruthy();
    const general = (await generalRes.json()) as { id: string };

    await page.goto(`${BASE}/r/${REPO_NAME}/~reviews/${reviewId}`);
    await expect(page.locator("[data-kbc-review-title]")).toContainText("e2e promote general refusal");

    // The button is still THERE (never hidden — root CLAUDE.md's honesty
    // rule: "a filter names what it hides") but disabled, with a caption
    // naming exactly why.
    const toggle = page.locator(`[data-kbc-promote-finding-toggle="${general.id}"]`);
    await expect(toggle).toBeVisible({ timeout: 10_000 });
    await expect(toggle).toBeDisabled();
    await expect(toggle).toHaveAttribute("title", /general comments have no file location/);
  });
});
