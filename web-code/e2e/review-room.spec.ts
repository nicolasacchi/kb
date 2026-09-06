import { expect, test } from "@playwright/test";
import { FEATURE_BRANCH, FEATURE_FILE } from "./fixture-repo";
import { BASE, REPO_DIR, REPO_NAME } from "./helpers";

/// PRR-U8 — kb v0.39 "The PR Room," unit U8 (final polish + e2e). The
/// review-room MUST path, in one continuous flow: create a PR-less local
/// review via the existing `POST /api/reviews` API (same pattern
/// `reviews.spec.ts`/`review-diff-page.spec.ts` already established —
/// `feature-x -> main`, never a throwaway branch, never touches `main`'s
/// tip), import two findings via the loopback `POST .../findings/import`
/// route (`review_findings.rs`'s `kbc-findings/1` wire shape), then PUT a
/// report so the Report tab has a verdict card. Drives the SPA through:
///
///  - Report tab renders the verdict card + both finding cards
///  - a disposition click round-trips (PUT, then the button goes active)
///  - the full-page diff shows the finding's thread + a severity gutter
///    tick, and `?finding=<slug>` scrolls+flashes it
///  - the overlay selector cycles findings visible → hidden → visible
///  - "Ask the agent" posts a review-level General thread
///    (`awaiting-agent`)
///  - marking a finding for publish + "Preview round →" opens the modal
///    (this review is local-only — no GitHub PR — so the preview's own
///    honest `not bound to a GitHub PR` state is the correct, deterministic
///    assertion here, never a live GitHub call)
///  - the Timeline tab lists the events this flow itself generated
///
/// No git-state choreography needed (no branch created, no restore in
/// `afterAll`) — this test only ever POSTs against the daemon's local
/// review/findings/report/annotation tables.

const BLOCKER_SLUG = "f-e2e-room-blocker";
const CONCERN_SLUG = "f-e2e-room-concern";

test.describe("review room — findings, disposition, diff overlay, ask, publish, timeline (PRR-U8)", () => {
  test("end to end", async ({ page, request }) => {
    test.skip(!REPO_DIR, "KB_CODE_E2E_REPO_DIR not set — global-setup didn't run");

    // --- create the review -------------------------------------------------
    const createRes = await request.post(`${BASE}/api/reviews`, {
      data: {
        repo: REPO_NAME,
        head_ref: FEATURE_BRANCH,
        base_ref: "main",
        title: "e2e review room",
      },
    });
    expect(createRes.ok(), `create review: ${createRes.status()} ${await createRes.text()}`).toBeTruthy();
    const created = (await createRes.json()) as { id: number };
    const reviewId = created.id;

    // --- import two findings (loopback) -------------------------------------
    const importRes = await request.post(`${BASE}/api/reviews/${reviewId}/findings/import`, {
      data: {
        schema: "kbc-findings/1",
        findings: [
          {
            slug: BLOCKER_SLUG,
            severity: "blocker",
            category: "Correctness",
            location: { path: FEATURE_FILE, kind: "single", lines: [2] },
            title: "e2e blocker finding",
            rationale: "e2e fixture blocker rationale.",
            recommendation: "e2e fixture recommendation.",
          },
          {
            // whole_file — deliberately NOT anchored to a line (the diff
            // overlay only ever renders LINE-anchored findings; this one
            // exercises the Report tab + side panel instead, see
            // `lib/reviewComments.ts`'s `indexThreads` doc on why a
            // whole_file finding's `resolution.line` is always null).
            slug: CONCERN_SLUG,
            severity: "concern",
            category: "Style",
            location: { path: FEATURE_FILE, kind: "whole_file" },
            title: "e2e concern finding",
            rationale: "e2e fixture concern rationale.",
          },
        ],
      },
    });
    expect(importRes.ok(), `import findings: ${importRes.status()} ${await importRes.text()}`).toBeTruthy();
    const imported = (await importRes.json()) as { created: string[] };
    expect([...imported.created].sort()).toEqual([BLOCKER_SLUG, CONCERN_SLUG].sort());

    // --- a report, so the Report tab defaults + the verdict card renders ---
    const reportRes = await request.put(`${BASE}/api/reviews/${reviewId}/report`, {
      data: {
        schema: "kbc-report/1",
        deck: "e2e review room deck",
        summary: "## Summary\n\ne2e report summary text.",
        verdict: "concern",
        verdict_headline: "e2e review needs a look",
        risk_score: 5,
      },
    });
    expect(reportRes.ok(), `put report: ${reportRes.status()} ${await reportRes.text()}`).toBeTruthy();

    // --- cockpit: Report tab auto-selected, verdict card + finding cards ---
    await page.goto(`${BASE}/r/${REPO_NAME}/~reviews/${reviewId}`);
    await expect(page.locator("[data-kbc-review-title]")).toContainText("e2e review room");
    const reportPanel = page.locator(`[data-kbc-report-panel="${reviewId}"]`);
    await expect(reportPanel).toBeVisible({ timeout: 10_000 });
    // `AgentVerdictCard` renders TWICE on this page (the header's own
    // "verdict dialectic strip" AND the Report tab's own copy,
    // `ReviewHeader.tsx` + `ReportPanel.tsx`) — scope to the Report tab's.
    await expect(reportPanel.locator("[data-kbc-agent-verdict]")).toBeVisible();
    const blockerCard = reportPanel.locator(`[data-kbc-finding="${BLOCKER_SLUG}"]`);
    const concernCard = reportPanel.locator(`[data-kbc-finding="${CONCERN_SLUG}"]`);
    await expect(blockerCard).toBeVisible();
    await expect(concernCard).toBeVisible();

    // --- disposition click round-trips (scoped to the blocker card, since
    // BOTH cards render their own "agree" button) --------------------------
    const agreeBtn = blockerCard.locator('[data-kbc-finding-disposition-btn="agree"]');
    await agreeBtn.click();
    await expect(agreeBtn).toHaveAttribute("aria-pressed", "true", { timeout: 10_000 });
    await expect(agreeBtn).toHaveClass(/kbc-dispo--active-agree/);

    // --- full-page diff: finding thread + severity tick, ?finding= flash --
    // V70-H1 — `.kbc-rdiff__flash` is TRANSIENT (`ReviewDiff.tsx`'s
    // `focusThreadId` effect adds it, then `setTimeout(…, 1400)` removes
    // it) and, on this `?finding=` path, gated behind TWO queries
    // (`commentsQ` for the thread, `findingsQ` to resolve the slug ->
    // annotation id) rather than the `?line=&side=` path's one — so under
    // host load the class can be added AND cleared entirely in the gap
    // between the preceding `findingThread`/`thread-mark` polls resolving
    // and this assertion's own first poll, which no amount of `toBeVisible`
    // timeout can recover (there is nothing left to find). An init script
    // installs a MutationObserver before any app code runs, so the
    // transient class is caught the instant it is added, however slow the
    // test runner's own polling cadence is.
    await page.addInitScript(() => {
      (window as unknown as { __kbcSawFlash?: boolean }).__kbcSawFlash = false;
      const arm = () => {
        const obs = new MutationObserver((muts) => {
          for (const m of muts) {
            const el = m.target as Element;
            if (el.classList?.contains("kbc-rdiff__flash")) {
              (window as unknown as { __kbcSawFlash?: boolean }).__kbcSawFlash = true;
            }
          }
        });
        obs.observe(document.body, { attributes: true, attributeFilter: ["class"], subtree: true });
      };
      if (document.body) arm();
      else document.addEventListener("DOMContentLoaded", arm, { once: true });
    });
    await page.goto(`${BASE}/r/${REPO_NAME}/~reviews/${reviewId}/diff?finding=${BLOCKER_SLUG}`);
    await expect(page.locator("[data-kbc-rdiff]")).toBeVisible({ timeout: 10_000 });
    const findingThread = page.locator(`[data-kbc-finding-slug="${BLOCKER_SLUG}"]`);
    await expect(findingThread).toBeVisible({ timeout: 10_000 });
    await expect(page.locator('[data-kbc-thread-mark="blocker"]')).toBeVisible();
    await page.waitForFunction(
      () => (window as unknown as { __kbcSawFlash?: boolean }).__kbcSawFlash === true,
      { timeout: 10_000 },
    );

    // --- overlay cycle hides/shows the finding thread -----------------------
    const overlaySelect = page.locator("[data-kbc-rdiff-overlay-select]");
    await overlaySelect.selectOption("findings");
    await expect(findingThread).toBeVisible();
    await overlaySelect.selectOption("none");
    await expect(findingThread).not.toBeVisible();
    await overlaySelect.selectOption("all");
    await expect(findingThread).toBeVisible();

    // --- back to the cockpit: ask the agent posts a General thread ---------
    await page.goto(`${BASE}/r/${REPO_NAME}/~reviews/${reviewId}`);
    await expect(page.locator("[data-kbc-ask-agent]")).toBeVisible({ timeout: 10_000 });
    const askBody = "e2e question: what about the overall approach here?";
    await page.locator("[data-kbc-ask-agent-body]").fill(askBody);
    await page.locator("[data-kbc-ask-agent-submit]").click();
    await expect(page.getByText(askBody)).toBeVisible({ timeout: 10_000 });
    await expect(page.locator('[data-kbc-review-ann-path=""]')).toBeVisible();
    await expect(page.locator('[data-kbc-question-state="awaiting-agent"]')).toBeVisible();

    // --- mark for publish, preview opens with the marked item ---------------
    const reportPanel2 = page.locator(`[data-kbc-report-panel="${reviewId}"]`);
    await expect(reportPanel2).toBeVisible({ timeout: 10_000 });
    const blockerMark = reportPanel2
      .locator(`[data-kbc-finding="${BLOCKER_SLUG}"]`)
      .locator(`[data-kbc-publish-mark="${BLOCKER_SLUG}"]`);
    await blockerMark.click();
    await expect(page.locator("[data-kbc-publish-marked-count]")).toContainText("1 marked", {
      timeout: 10_000,
    });
    await page.locator("[data-kbc-publish-preview-open]").click();
    await expect(page.locator("[data-kbc-publish-preview]")).toBeVisible({ timeout: 10_000 });
    // Local-only review (no GitHub PR bound) — the preview honestly says so
    // rather than fabricating an exported item list against a PR that
    // doesn't exist (`PublishPreview.tsx`'s own `prBound` gate; the SPA
    // NEVER talks to GitHub — root CLAUDE.md's non-goal list).
    await expect(page.locator("[data-kbc-publish-preview-no-pr]")).toBeVisible();
    await page.locator(".kbc-publish-preview .confirm__cancel").click();
    await expect(page.locator("[data-kbc-publish-preview]")).toHaveCount(0);

    // --- Timeline tab lists the events this flow itself generated ----------
    await page.locator('[data-kbc-review-view="timeline"]').click();
    await expect(page.locator(`[data-kbc-timeline="${reviewId}"]`)).toBeVisible({ timeout: 10_000 });
    await expect(page.locator('[data-kbc-timeline-row="review_created"]')).toBeVisible();
    await expect(page.locator('[data-kbc-timeline-row="findings_import"]')).toBeVisible();
    await expect(page.locator('[data-kbc-timeline-row="disposition"]')).toBeVisible();
  });
});
