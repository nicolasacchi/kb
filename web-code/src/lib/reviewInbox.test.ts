import { describe, expect, it } from "vitest";
import type { PrOut, ReviewInboxRow, ReviewSummaryPr } from "../api/types";
import {
  boundPrNumbers,
  buildCreateReviewPrInput,
  inboxDotTone,
  inboxReasonChips,
  joinInboxRows,
  reviewByPrNumber,
  unreviewedPrs,
} from "./reviewInbox";

function inboxRow(overrides: Partial<ReviewInboxRow> = {}): ReviewInboxRow {
  return {
    review_id: 1,
    repo: "acme/widgets",
    pr_number: 42,
    title: "Fix the thing",
    unresolved_findings: 0,
    unanswered_questions: 0,
    verdict: null,
    verdict_stale: false,
    pr_head_drift: null,
    updated_at: 1000,
    ...overrides,
  };
}

function reviewSummary(overrides: Partial<ReviewSummaryPr> = {}): ReviewSummaryPr {
  return {
    id: 1,
    repo: "acme/widgets",
    title: "Fix the thing",
    base_ref: "main",
    head_ref: "fix-thing",
    session_id: null,
    state: "open",
    created_at: 900,
    updated_at: 1000,
    latest_ps: 1,
    files_count: 3,
    viewed_count: 0,
    open_annotations: 0,
    verdict: null,
    verdict_stale: false,
    ...overrides,
  };
}

function pr(overrides: Partial<PrOut> = {}): PrOut {
  return {
    number: 1,
    title: "A pull request",
    author: "octocat",
    head_ref: "feature",
    base_ref: "main",
    updated_at: "2026-08-01T00:00:00Z",
    draft: false,
    ...overrides,
  };
}

describe("inboxDotTone", () => {
  it("is hollow when there's no report", () => {
    expect(inboxDotTone(false, null)).toBe("hollow");
    expect(inboxDotTone(undefined, 7)).toBe("hollow");
  });

  it("is hollow when has_report is true but the score is missing/non-finite", () => {
    expect(inboxDotTone(true, null)).toBe("hollow");
    expect(inboxDotTone(true, Number.NaN)).toBe("hollow");
  });

  it("buckets green below 4, warn 4..6, red 7+", () => {
    expect(inboxDotTone(true, 0)).toBe("green");
    expect(inboxDotTone(true, 3)).toBe("green");
    expect(inboxDotTone(true, 4)).toBe("warn");
    expect(inboxDotTone(true, 6)).toBe("warn");
    expect(inboxDotTone(true, 7)).toBe("red");
    expect(inboxDotTone(true, 10)).toBe("red");
  });
});

describe("inboxReasonChips", () => {
  it("is empty for a quiet row (no findings, no questions, no drift, no staleness)", () => {
    expect(inboxReasonChips(inboxRow())).toEqual([]);
  });

  it("orders awaiting-you first, then unresolved findings, then drift, then verdict-stale", () => {
    const chips = inboxReasonChips(
      inboxRow({
        unanswered_questions: 1,
        unresolved_findings: 2,
        pr_head_drift: true,
        verdict_stale: true,
      }),
    );
    expect(chips.map((c) => c.key)).toEqual(["awaiting", "unresolved", "head_drift", "verdict_stale"]);
  });

  it("singularizes 'finding' at exactly 1", () => {
    expect(inboxReasonChips(inboxRow({ unresolved_findings: 1 }))[0]?.label).toBe("1 unresolved finding");
    expect(inboxReasonChips(inboxRow({ unresolved_findings: 2 }))[0]?.label).toBe("2 unresolved findings");
  });

  it("renders the awaiting-you count verbatim from the row's own term", () => {
    expect(inboxReasonChips(inboxRow({ unanswered_questions: 3 }))[0]?.label).toBe("❓ 3 awaiting you");
  });

  it("never emits a chip for a false/zero term", () => {
    const chips = inboxReasonChips(inboxRow({ pr_head_drift: false, verdict_stale: false }));
    expect(chips.some((c) => c.key === "head_drift" || c.key === "verdict_stale")).toBe(false);
  });
});

describe("joinInboxRows", () => {
  it("pulls has_report/report_risk_score from the matching reviews-list row", () => {
    const rows = joinInboxRows(
      [inboxRow({ review_id: 7, updated_at: 12345 })],
      [reviewSummary({ id: 7, has_report: true, report_risk_score: 8 })],
    );
    expect(rows).toHaveLength(1);
    expect(rows[0]).toMatchObject({
      reviewId: 7,
      hasReport: true,
      riskScore: 8,
      dot: "red",
      updatedAt: 12345,
    });
  });

  it("degrades to hollow/null when the review row has no join match", () => {
    const rows = joinInboxRows([inboxRow({ review_id: 99 })], []);
    expect(rows[0]).toMatchObject({ hasReport: false, riskScore: null, dot: "hollow" });
  });

  it("falls back the title to PR number, then review id, when title is blank", () => {
    const [withPr] = joinInboxRows([inboxRow({ title: null, pr_number: 55 })], []);
    expect(withPr?.title).toBe("PR #55");
    const [withoutPr] = joinInboxRows([inboxRow({ title: "  ", pr_number: null, review_id: 9 })], []);
    expect(withoutPr?.title).toBe("review #9");
  });

  it("carries pr author/draft from the joined row's pr_meta", () => {
    const rows = joinInboxRows(
      [inboxRow({ review_id: 3 })],
      [reviewSummary({ id: 3, pr_meta: { author: "hubot", draft: true } })],
    );
    expect(rows[0]).toMatchObject({ prAuthor: "hubot", prDraft: true });
  });

  it("preserves the inbox route's own server-side order (never re-sorts)", () => {
    const rows = joinInboxRows(
      [inboxRow({ review_id: 3 }), inboxRow({ review_id: 1 }), inboxRow({ review_id: 2 })],
      [],
    );
    expect(rows.map((r) => r.reviewId)).toEqual([3, 1, 2]);
  });
});

describe("boundPrNumbers / unreviewedPrs / reviewByPrNumber", () => {
  it("counts a PR bound to a CLOSED review as reviewed, not unreviewed", () => {
    const reviews = [reviewSummary({ id: 1, pr_number: 42, state: "closed" })];
    expect(boundPrNumbers(reviews)).toEqual(new Set([42]));
    expect(unreviewedPrs([pr({ number: 42 })], reviews)).toEqual([]);
  });

  it("keeps a PR with no bound review in the unreviewed set, in list order", () => {
    const reviews = [reviewSummary({ id: 1, pr_number: 1 })];
    const prs = [pr({ number: 1 }), pr({ number: 2 }), pr({ number: 3 })];
    expect(unreviewedPrs(prs, reviews).map((p) => p.number)).toEqual([2, 3]);
  });

  it("ignores reviews with no pr_number at all", () => {
    const reviews = [reviewSummary({ id: 1, pr_number: undefined })];
    expect(boundPrNumbers(reviews)).toEqual(new Set());
  });

  it("reviewByPrNumber resolves the FIRST bound review deterministically on a duplicate", () => {
    const reviews = [
      reviewSummary({ id: 1, pr_number: 42 }),
      reviewSummary({ id: 2, pr_number: 42 }),
    ];
    expect(reviewByPrNumber(reviews).get(42)?.id).toBe(1);
  });
});

describe("buildCreateReviewPrInput", () => {
  it("carries repo, pr_number, and base_ref straight from the PR row", () => {
    expect(buildCreateReviewPrInput("acme/widgets", pr({ number: 7, base_ref: "develop" }))).toEqual({
      repo: "acme/widgets",
      pr_number: 7,
      base_ref: "develop",
    });
  });
});
