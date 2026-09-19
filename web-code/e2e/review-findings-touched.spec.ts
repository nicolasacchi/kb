import { execFileSync } from "node:child_process";
import { writeFileSync } from "node:fs";
import { join } from "node:path";
import { expect, test } from "@playwright/test";
import { BASE, REPO_DIR, REPO_NAME } from "./helpers";

/// V80-F3 — "lines changed in ps N": `touched_in` on `GET
/// /api/reviews/{id}/findings`, the finding-card caption chip, and the
/// Timeline row it derives client-side. Follows `review-diff-v2.spec.ts`'s
/// pattern exactly — its own disposable base+branch (never `main`, never
/// `FEATURE_BRANCH`, which other specs assume is untouched), a SECOND
/// patchset via the `POST /api/reviews/{id}/snapshot` route, cleaned up in
/// `afterAll`.
///
/// The fixture repo `review-room.spec.ts`/`review-diff-v2.spec.ts` share
/// only ever has ONE patchset per review in the rest of this suite — this
/// is the harness's first two-patchset findings scenario (per this unit's
/// brief: "add a second patchset in the spec via the existing review
/// snapshot route and say so" — done here).

const BASE_BRANCH = "e2e-f3-base";
const HEAD_BRANCH = "e2e-f3";
const FILE = "e2e_f3.rs";
const BLOCKER_SLUG = "f-e2e-f3-blocker";

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

/// A 20-line file so a `-U3` hunk around one changed line never touches
/// the file's edges (the exact/adjacent math this unit's server-side unit
/// tests pin depends on real context padding, not an accident of a
/// 4-line fixture file). `line2`/`line10` let ps1 carry an UNRELATED edit
/// (so the review's own first patchset isn't a no-op diff, matching
/// `review-diff-v2.spec.ts`'s own precedent) while the finding's cited
/// line 10 stays untouched until ps2.
function fileLines(line2: string, line10: string): string {
  const lines = Array.from({ length: 20 }, (_, i) => `// f3 line ${i + 1}`);
  lines[1] = line2; // line 2 (1-based)
  lines[9] = line10; // line 10 (1-based)
  return `${lines.join("\n")}\n`;
}

test.describe("review findings — touched_in (V80-F3)", () => {
  test.afterAll(() => {
    tryGit(["checkout", "main"]);
    tryGit(["branch", "-D", HEAD_BRANCH]);
    tryGit(["branch", "-D", BASE_BRANCH]);
  });

  test("a later patchset's diff surfaces as touched_in — API + chip + timeline", async ({
    page,
    request,
  }) => {
    test.skip(!REPO_DIR, "KB_CODE_E2E_REPO_DIR not set — global-setup didn't run");

    tryGit(["checkout", "main"]);
    tryGit(["branch", "-D", HEAD_BRANCH]);
    tryGit(["branch", "-D", BASE_BRANCH]);

    git(["checkout", "-q", "-b", BASE_BRANCH, "main"]);
    writeFileSync(join(REPO_DIR, FILE), fileLines("// f3 line 2", "fn f3_target() -> i32 { 1 }"));
    git(["add", FILE]);
    git(["commit", "-q", "-m", "e2e f3 base file"]);

    // ps1 carries a real but UNRELATED edit (line 2) — matches
    // `review-diff-v2.spec.ts`'s own precedent of a non-empty first
    // patchset — while the finding's cited line 10 stays untouched, so
    // `own_ps` (patchset 1) reads the SAME line-10 content the base has.
    git(["checkout", "-q", "-b", HEAD_BRANCH, BASE_BRANCH]);
    writeFileSync(
      join(REPO_DIR, FILE),
      fileLines("// f3 line 2 EDITED", "fn f3_target() -> i32 { 1 }"),
    );
    git(["add", FILE]);
    git(["commit", "-q", "-m", "e2e f3 ps1 — unrelated edit"]);
    git(["checkout", "-q", "main"]);

    const createRes = await request.post(`${BASE}/api/reviews`, {
      data: { repo: REPO_NAME, head_ref: HEAD_BRANCH, base_ref: BASE_BRANCH, title: "e2e f3 touched_in" },
    });
    expect(createRes.ok(), `create review: ${createRes.status()} ${await createRes.text()}`).toBeTruthy();
    const reviewId = ((await createRes.json()) as { id: number }).id;

    // --- import a finding citing line 10 (against ps1) ---------------------
    const importRes = await request.post(`${BASE}/api/reviews/${reviewId}/findings/import`, {
      data: {
        schema: "kbc-findings/1",
        findings: [
          {
            slug: BLOCKER_SLUG,
            severity: "blocker",
            category: "Correctness",
            location: { path: FILE, kind: "single", lines: [10] },
            title: "e2e f3 finding",
            rationale: "e2e f3 fixture rationale.",
          },
        ],
      },
    });
    expect(importRes.ok(), `import: ${importRes.status()} ${await importRes.text()}`).toBeTruthy();

    // --- a report, so the Report tab auto-selects (same trick
    // review-room.spec.ts uses) -----------------------------------------
    const reportRes = await request.put(`${BASE}/api/reviews/${reviewId}/report`, {
      data: {
        schema: "kbc-report/1",
        deck: "e2e f3 deck",
        summary: "## Summary\n\ne2e f3 report summary.",
        verdict: "concern",
        verdict_headline: "e2e f3 needs a look",
        risk_score: 5,
      },
    });
    expect(reportRes.ok(), `put report: ${reportRes.status()} ${await reportRes.text()}`).toBeTruthy();

    // --- a SECOND patchset: change line 10 itself (exact overlap) --------
    git(["checkout", "-q", HEAD_BRANCH]);
    writeFileSync(
      join(REPO_DIR, FILE),
      fileLines("// f3 line 2 EDITED", "fn f3_target() -> i32 { 2 }"),
    );
    git(["commit", "-aq", "-m", "e2e f3 ps2 — touch line 10"]);
    git(["checkout", "-q", "main"]);
    const snapRes = await request.post(`${BASE}/api/reviews/${reviewId}/snapshot`, { data: {} });
    expect(snapRes.ok(), `snapshot: ${snapRes.status()} ${await snapRes.text()}`).toBeTruthy();

    // --- API: touched_in names ps2, exact ---------------------------------
    const findingsRes = await request.get(`${BASE}/api/reviews/${reviewId}/findings`);
    expect(findingsRes.ok()).toBeTruthy();
    const findingsBody = (await findingsRes.json()) as {
      findings: Array<{
        slug: string;
        own_ps: number | null;
        touched_in: Array<{ ps: number; hunks: number; overlap: string }>;
        touched_in_capped: boolean;
      }>;
    };
    const blocker = findingsBody.findings.find((f) => f.slug === BLOCKER_SLUG);
    expect(blocker, "blocker finding present").toBeTruthy();
    expect(blocker?.own_ps).toBe(1);
    expect(blocker?.touched_in_capped).toBe(false);
    expect(blocker?.touched_in).toEqual([{ ps: 2, hunks: 1, overlap: "exact" }]);

    // --- SPA: the finding card's chip -------------------------------------
    await page.goto(`${BASE}/r/${REPO_NAME}/~reviews/${reviewId}`);
    const reportPanel = page.locator(`[data-kbc-report-panel="${reviewId}"]`);
    await expect(reportPanel).toBeVisible({ timeout: 10_000 });
    const card = reportPanel.locator(`[data-kbc-finding="${BLOCKER_SLUG}"]`);
    await expect(card).toBeVisible();
    const chip = card.locator('[data-kbc-finding-touch-ps="2"]');
    await expect(chip).toBeVisible();
    await expect(chip).toHaveText("lines changed in ps 2");
    await expect(chip).toHaveAttribute("title", /exact overlap — 1 hunk in ps 2's diff from ps 1/);
    const chipHref = await chip.getAttribute("href");
    expect(chipHref).toContain("ps=1..2");
    expect(chipHref).toContain(encodeURIComponent(FILE));

    // --- SPA: the Timeline tab's derived row --------------------------------
    await page.goto(`${BASE}/r/${REPO_NAME}/~reviews/${reviewId}?tab=timeline`);
    const timelineRow = page.locator('[data-kbc-timeline-row="finding_touch"]');
    await expect(timelineRow.first()).toBeVisible({ timeout: 10_000 });
    await expect(timelineRow.first()).toContainText(`${BLOCKER_SLUG}'s lines in ps 2`);
  });
});
