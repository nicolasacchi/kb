// PRR-U1 — kb v0.39 "The PR Room," unit U1 (design-ui.md §2 S1): pure view-
// model derivation for the Review Room landing (`routes/Reviews.tsx`) and
// the unbound-PR strip (`routes/Prs.tsx` / `components/reviews/
// UnreviewedPrsStrip.tsx`). No fetching, no React — every function here
// takes already-fetched wire rows and returns plain data, so it's testable
// with zero mocking (`reviewInbox.test.ts`).
//
// The inbox route (`GET /api/reviews/inbox`, `review_inbox.rs`) is
// deliberately CHEAP: it never re-derives or echoes the agent report's own
// `blocker|concern|ok` verdict enum, only `has_report`/`report_risk_score`
// (a bare parse of the stored blob — see `pr_binding_and_report_fields`'s
// own doc, "never a daemon-authored verdict of its own"). This module joins
// each inbox row against the ALREADY-FETCHED reviews list (`ReviewSummaryPr`,
// R4's additive fields) for those report-summary fields — no second live
// call per row, matching the inbox route's own "no N-request storm" design
// constraint (its module doc, on `pr_head_drift`).

import type { PrOut, ReviewInboxRow, ReviewSummaryPr } from "../api/types";

// --- Inbox rows -------------------------------------------------------------

export type InboxDotTone = "red" | "warn" | "green" | "hollow";

/// Bucket a report's `risk_score` (0..10) into the same red/warn/green
/// severity vocabulary the report cards use. The inbox row itself never
/// carries the report's authored `verdict` enum (see this module's own
/// doc), so this bucketing is the landing page's OWN documented proxy for
/// "the agent verdict color" the design mock calls for — not a re-read of
/// an authored verdict string. `hollow` (no fill) whenever there's no
/// report to summarize, regardless of score.
export function inboxDotTone(hasReport: boolean | undefined, riskScore: number | null | undefined): InboxDotTone {
  if (!hasReport || riskScore == null || !Number.isFinite(riskScore)) return "hollow";
  if (riskScore >= 7) return "red";
  if (riskScore >= 4) return "warn";
  return "green";
}

export interface InboxReasonChip {
  key: "awaiting" | "unresolved" | "head_drift" | "verdict_stale";
  label: string;
}

/// Reason chips derived STRICTLY from the row's own named terms — the same
/// terms `review_inbox::inbox_score` weighs (`unanswered_questions*2 +
/// unresolved_findings`), plus the two staleness flags the row also
/// carries. Never a re-judgement; every chip traces to one field on `row`.
/// Order mirrors the score's own weighting: the ×2 term (awaiting-you)
/// first, then unresolved findings, then the two "the room might be lying
/// to you" staleness flags — see invariant "the room never lies" (design
/// doc §0).
export function inboxReasonChips(row: ReviewInboxRow): InboxReasonChip[] {
  const chips: InboxReasonChip[] = [];
  if (row.unanswered_questions > 0) {
    chips.push({
      key: "awaiting",
      label: `❓ ${row.unanswered_questions} awaiting you`,
    });
  }
  if (row.unresolved_findings > 0) {
    chips.push({
      key: "unresolved",
      label: `${row.unresolved_findings} unresolved finding${row.unresolved_findings === 1 ? "" : "s"}`,
    });
  }
  if (row.pr_head_drift) {
    chips.push({ key: "head_drift", label: "PR head moved" });
  }
  if (row.verdict_stale) {
    chips.push({ key: "verdict_stale", label: "verdict stale" });
  }
  return chips;
}

/// `ReviewSummaryPr` rows keyed by `id` — the join table `joinInboxRows`
/// (and the PR-binding helpers below) read from.
export function reviewsById(reviews: readonly ReviewSummaryPr[]): Map<number, ReviewSummaryPr> {
  return new Map(reviews.map((r) => [r.id, r]));
}

export interface InboxViewRow {
  reviewId: number;
  repo: string;
  prNumber: number | null;
  /// `row.title` when set, else a PR-number or review-id fallback — a row
  /// must always have SOME label, never a blank link.
  title: string;
  dot: InboxDotTone;
  riskScore: number | null;
  hasReport: boolean;
  unresolvedFindings: number;
  unansweredQuestions: number;
  verdict: ReviewInboxRow["verdict"];
  verdictStale: boolean;
  prHeadDrift: boolean | null;
  updatedAt: number;
  chips: InboxReasonChip[];
  prAuthor: string | null;
  prDraft: boolean;
}

function inboxRowTitle(row: ReviewInboxRow): string {
  const trimmed = row.title?.trim();
  if (trimmed) return trimmed;
  if (row.pr_number != null) return `PR #${row.pr_number}`;
  return `review #${row.review_id}`;
}

/// The landing page's core join: `GET /api/reviews/inbox` rows (already
/// server-side ranked — `review_inbox::sort_inbox_rows`, never re-sorted
/// here) extended with `pr_meta`/`has_report`/`report_risk_score` pulled
/// from the matching `ReviewSummaryPr` row when present. A missing join
/// (an inbox row whose review vanished from the list between the two
/// fetches, or an older daemon without R4's fields) degrades to `hollow`/
/// `riskScore: null` — never thrown, never dropped from the queue.
export function joinInboxRows(
  inbox: readonly ReviewInboxRow[],
  reviews: readonly ReviewSummaryPr[],
): InboxViewRow[] {
  const byId = reviewsById(reviews);
  return inbox.map((row) => {
    const joined = byId.get(row.review_id);
    const hasReport = joined?.has_report ?? false;
    const riskScore = typeof joined?.report_risk_score === "number" ? joined.report_risk_score : null;
    return {
      reviewId: row.review_id,
      repo: row.repo,
      prNumber: row.pr_number,
      title: inboxRowTitle(row),
      dot: inboxDotTone(hasReport, riskScore),
      riskScore,
      hasReport,
      unresolvedFindings: row.unresolved_findings,
      unansweredQuestions: row.unanswered_questions,
      verdict: row.verdict,
      verdictStale: row.verdict_stale,
      prHeadDrift: row.pr_head_drift,
      updatedAt: row.updated_at,
      chips: inboxReasonChips(row),
      prAuthor: joined?.pr_meta?.author ?? null,
      prDraft: joined?.pr_meta?.draft ?? false,
    };
  });
}

// --- Unreviewed-PR strip -----------------------------------------------------

/// Every PR number already bound to SOME review (open or closed — a PR
/// reviewed-then-closed is still "reviewed", not an unreviewed gap).
export function boundPrNumbers(reviews: readonly ReviewSummaryPr[]): Set<number> {
  const bound = new Set<number>();
  for (const r of reviews) {
    if (r.pr_number != null) bound.add(r.pr_number);
  }
  return bound;
}

/// `PrOut` rows with no local review bound to their number — the secondary
/// strip's source list (design doc §2 S1: "PRs with no local review are NOT
/// in the inbox route"). Order preserved from `prs` (the GitHub list's own
/// `updated_at`-ish server order).
export function unreviewedPrs(prs: readonly PrOut[], reviews: readonly ReviewSummaryPr[]): PrOut[] {
  const bound = boundPrNumbers(reviews);
  return prs.filter((pr) => !bound.has(pr.number));
}

/// First review bound to each PR number — `routes/Prs.tsx`'s "review
/// exists → Open room" join. When more than one review is (improbably)
/// bound to the same number, the first one wins deterministically (list
/// order), never an arbitrary pick per render.
export function reviewByPrNumber(reviews: readonly ReviewSummaryPr[]): Map<number, ReviewSummaryPr> {
  const byNumber = new Map<number, ReviewSummaryPr>();
  for (const r of reviews) {
    if (r.pr_number != null && !byNumber.has(r.pr_number)) byNumber.set(r.pr_number, r);
  }
  return byNumber;
}

/// `POST /api/reviews/pr` body for a given PR row — the ONE place "Start
/// review" builds its request, shared by `Prs.tsx` and
/// `UnreviewedPrsStrip.tsx` so the two CTAs can never drift on which fields
/// they send.
export function buildCreateReviewPrInput(repo: string, pr: Pick<PrOut, "number" | "base_ref">) {
  return { repo, pr_number: pr.number, base_ref: pr.base_ref };
}
