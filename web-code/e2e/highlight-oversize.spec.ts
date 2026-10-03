import { expect, test } from "@playwright/test";
import { FEATURE_BRANCH, FEATURE_FILE } from "./fixture-repo";
import { BASE, REPO_DIR, REPO_NAME } from "./helpers";

/// v0.44 X2 (A10.f2) - an OVERSIZE highlight snippet renders plain, never
/// pending, and does not take its sibling's paint down with it.
///
/// `POST /api/highlight/batch` refuses the WHOLE batch with a 400 when any one
/// item is over 256 KiB (`MAX_SNIPPET_BYTES`). `useHighlight` therefore drops
/// an oversize item client-side and reports it through `unpaintableIds`;
/// `HighlightedSnippet` then settles on `data-kbc-hl-tier="plain"`. The
/// unit tests pin the pieces; this drives the real page: two findings on one
/// Room, each carrying an evidence snippet, one of them 256 KiB + 1 of
/// valid Rust.

const OVERSIZE_BYTES = 256 * 1024 + 1;
const BIG = "oversize-finding";
const SMALL = "small-finding";

function oversizeRust(): string {
  const line = "fn pad() -> i32 { 1 }\n";
  return line.repeat(Math.ceil(OVERSIZE_BYTES / line.length));
}

test.describe("oversize highlight snippet (A10.f2)", () => {
  test("renders plain, never pending; the sibling still paints", async ({ page, request }) => {
    test.skip(!REPO_DIR, "KB_CODE_E2E_REPO_DIR not set - global-setup didn't run");

    const createRes = await request.post(`${BASE}/api/reviews`, {
      data: {
        repo: REPO_NAME,
        head_ref: FEATURE_BRANCH,
        base_ref: "main",
        title: "e2e oversize highlight",
      },
    });
    expect(createRes.ok(), `create review: ${createRes.status()} ${await createRes.text()}`).toBeTruthy();
    const { id: reviewId } = (await createRes.json()) as { id: number };

    const source = oversizeRust();
    expect(new TextEncoder().encode(source).length).toBeGreaterThan(256 * 1024);

    const importRes = await request.post(`${BASE}/api/reviews/${reviewId}/findings/import`, {
      data: {
        schema: "kbc-findings/1",
        findings: [
          {
            slug: BIG,
            severity: "concern",
            category: "Correctness",
            location: { path: FEATURE_FILE, kind: "single", lines: [1] },
            title: "e2e oversize evidence",
            rationale: "The evidence snippet is over the highlight ceiling.",
            evidence: { lang: "rust", source },
          },
          {
            slug: SMALL,
            severity: "concern",
            category: "Correctness",
            location: { path: FEATURE_FILE, kind: "single", lines: [1] },
            title: "e2e small evidence",
            rationale: "The evidence snippet is small and valid Rust.",
            evidence: { lang: "rust", source: "fn small() -> i32 {\n    1\n}\n" },
          },
        ],
      },
    });
    expect(importRes.ok(), `import: ${importRes.status()} ${await importRes.text()}`).toBeTruthy();

    // A report makes the Room open on the Report tab, where finding cards live.
    const reportRes = await request.put(`${BASE}/api/reviews/${reviewId}/report`, {
      data: {
        schema: "kbc-report/1",
        deck: "e2e oversize deck",
        summary: "## Summary\n\ne2e oversize.",
        verdict: "concern",
        verdict_headline: "e2e oversize",
        risk_score: 3,
      },
    });
    expect(reportRes.ok(), `put report: ${reportRes.status()} ${await reportRes.text()}`).toBeTruthy();

    await page.goto(`${BASE}/r/${REPO_NAME}/~reviews/${reviewId}`);
    const big = page.locator(`[data-kbc-finding="${BIG}"] [data-kbc-hl-snippet]`);
    const small = page.locator(`[data-kbc-finding="${SMALL}"] [data-kbc-hl-snippet]`);

    // The sibling paints: its tier settles on a painted value ...
    await expect(small).toHaveAttribute("data-kbc-hl-tier", /^(?!pending$|plain$|none$).+/, { timeout: 20_000 });
    await expect(small.locator("[data-kbc-hl]").first()).toBeVisible();
    // ... and the oversize one is PLAIN, not stuck pending, with no spans.
    await expect(big).toHaveAttribute("data-kbc-hl-tier", "plain", { timeout: 20_000 });
    await expect(big.locator("[data-kbc-hl]")).toHaveCount(0);
  });
});
