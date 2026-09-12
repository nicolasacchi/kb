import { expect, test } from "@playwright/test";
import { FEATURE_BRANCH, FEATURE_FILE } from "./fixture-repo";
import { BASE, REPO_DIR, REPO_NAME } from "./helpers";

/// V76-B3 — kbc-prose/1. A finding whose rationale cites `path:line` opens
/// the reader at that line; an `f-slug` mention focuses the finding card
/// (the rail row). Playwright cannot run in the builder sandbox; CI is the
/// gate. Hooks this spec drives: `data-kbc-finding`, `data-kbc-finding-rationale`,
/// `data-kbc-prose-ref`, `data-kbc-prose-focused`, `data-kbc-report-panel`.

const PATH_SLUG = "f-e2e-prose-path";
const MENTION_SLUG = "f-e2e-prose-mention";

test.describe("prose refs (kbc-prose/1)", () => {
  test("a path:line opens the reader at the line; an f-slug mention focuses the rail row", async ({
    page,
    request,
  }) => {
    test.skip(!REPO_DIR, "KB_CODE_E2E_REPO_DIR not set — global-setup didn't run");

    const createRes = await request.post(`${BASE}/api/reviews`, {
      data: {
        repo: REPO_NAME,
        head_ref: FEATURE_BRANCH,
        base_ref: "main",
        title: "e2e prose refs",
      },
    });
    expect(createRes.ok(), `create: ${createRes.status()} ${await createRes.text()}`).toBeTruthy();
    const { id: reviewId } = (await createRes.json()) as { id: number };

    const importRes = await request.post(`${BASE}/api/reviews/${reviewId}/findings/import`, {
      data: {
        schema: "kbc-findings/1",
        findings: [
          {
            slug: PATH_SLUG,
            severity: "blocker",
            category: "Correctness",
            location: { path: FEATURE_FILE, kind: "single", lines: [1] },
            title: "prose path finding",
            rationale: `The bug is in ${FEATURE_FILE}:1.`,
          },
          {
            slug: MENTION_SLUG,
            severity: "concern",
            category: "Style",
            location: { path: FEATURE_FILE, kind: "whole_file" },
            title: "prose mention finding",
            rationale: `Same class of bug as ${PATH_SLUG} on the new path.`,
          },
        ],
      },
    });
    expect(importRes.ok(), `import: ${importRes.status()} ${await importRes.text()}`).toBeTruthy();

    const reportRes = await request.put(`${BASE}/api/reviews/${reviewId}/report`, {
      data: {
        schema: "kbc-report/1",
        deck: "e2e prose refs deck",
        summary: "e2e prose refs summary.",
        verdict: "concern",
        verdict_headline: "e2e prose refs",
        risk_score: 4,
      },
    });
    expect(reportRes.ok(), `put report: ${reportRes.status()} ${await reportRes.text()}`).toBeTruthy();

    await page.goto(`${BASE}/r/${REPO_NAME}/~reviews/${reviewId}`);
    const reportPanel = page.locator(`[data-kbc-report-panel="${reviewId}"]`);
    await expect(reportPanel).toBeVisible({ timeout: 15_000 });

    const mentionCard = reportPanel.locator(`[data-kbc-finding="${MENTION_SLUG}"]`);
    await expect(mentionCard).toBeVisible();
    const findingRef = mentionCard.locator('[data-kbc-prose-ref="finding"]');
    await expect(findingRef).toBeVisible({ timeout: 10_000 });
    await findingRef.click();
    await expect(reportPanel.locator(`[data-kbc-finding="${PATH_SLUG}"]`)).toHaveAttribute(
      "data-kbc-prose-focused",
      "",
    );

    const pathCard = reportPanel.locator(`[data-kbc-finding="${PATH_SLUG}"]`);
    const pathRef = pathCard.locator('[data-kbc-prose-ref="path"]');
    await expect(pathRef).toBeVisible();
    await pathRef.click();
    await expect(page).toHaveURL(new RegExp(`${FEATURE_FILE.replace(".", "\\.")}.*line=1`), {
      timeout: 10_000,
    });
  });
});
