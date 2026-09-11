// PRR-U2 §2 S2 — the header's PR row (mock's `.kbc-prchip-row`): PR number
// + external GitHub link, author, branches, draft state, CI summary. Reads
// from the review's OWN PR-binding snapshot (`ReviewDetailPr` — see
// `api/types.ts`'s doc: these fields aren't on the wire yet, a concurrent
// unit R4 is landing them) rather than issuing a live `GET /api/prs/{n}`
// call — the binding snapshot IS the "as of bind/last-fetch time" record
// design-server.md §1.2 documents, and `CiChecksCard` already owns the one
// live GitHub call this cockpit makes for CI.
import type { ReviewDetailPr } from "../../api/types";
import { summarizeChecks } from "./CiChecksCard";
import { useReviewChecks } from "../../hooks/useReviews";
import { Icon } from "../icons";

/// `owner/name` + PR number → the external `github.com` PR URL. `null` when
/// the slug isn't a real `owner/name` pair (`pr_repo_slug` degrades to the
/// raw origin URL or `"unknown"` for a non-GitHub origin — see
/// `create_review_pr`'s own doc) — a chip that can't build a real GitHub
/// link renders no external-link affordance rather than a broken one.
export function prExternalUrl(repoSlug: string | undefined, prNumber: number | undefined): string | null {
  if (!repoSlug || prNumber == null) return null;
  if (!/^[^/\s]+\/[^/\s]+$/.test(repoSlug)) return null;
  return `https://github.com/${repoSlug}/pull/${prNumber}`;
}

export interface PrChipProps {
  repo: string;
  reviewId: number;
  review: ReviewDetailPr;
}

/// Renders `null` entirely when the review isn't PR-bound (`pr_number`
/// absent) — a local review's header stays exactly as it is today (§8:
/// "no PR bound: header simply omits the PR chip row").
export default function PrChip({ repo, reviewId, review }: PrChipProps) {
  const checksQ = useReviewChecks(repo, reviewId, review.pr_number);
  if (review.pr_number == null) return null;

  const meta = review.pr_meta ?? null;
  const externalUrl = prExternalUrl(review.pr_repo_slug, review.pr_number);
  const summary = checksQ.data ? summarizeChecks(checksQ.data.checks) : null;

  return (
    <div className="kbc-prchip-row" data-kbc-pr-chip={review.pr_number}>
      {externalUrl ? (
        <a className="gh" href={externalUrl} target="_blank" rel="noreferrer">
          PR #{review.pr_number} <Icon.External />
        </a>
      ) : (
        <span className="gh">PR #{review.pr_number}</span>
      )}
      {meta ? (
        <>
          <span className="sep">·</span>
          <span>{meta.author ?? "unknown author"}</span>
          <span className="sep">·</span>
          <code>{meta.head_ref ?? review.head_ref}</code>
          <span aria-hidden="true">→</span>
          <code>{meta.base_ref ?? review.base_ref}</code>
          {meta.draft && (
            <>
              <span className="sep">·</span>
              <span className="kbc-chip kbc-chip--draft">draft</span>
            </>
          )}
        </>
      ) : (
        review.pr_meta_unavailable_reason && (
          <>
            <span className="sep">·</span>
            <span className="kbc-review__card-empty" data-kbc-pr-meta-unavailable>
              PR metadata unavailable
              {typeof review.pr_meta_unavailable_reason === "string"
                ? ` at import (${review.pr_meta_unavailable_reason})`
                : ` (${review.pr_meta_unavailable_reason.code}: ${review.pr_meta_unavailable_reason.hint})`}
            </span>
          </>
        )
      )}
      {summary && summary.total > 0 && (
        <>
          <span className="sep">·</span>
          <span className={`kbc-ci-chip kbc-ci-chip--${summary.worst ?? "pass"}`} data-kbc-pr-chip-ci={summary.worst ?? "pass"}>
            {summary.worst === "fail" ? "✗" : summary.worst === "pending" ? "…" : "✓"} CI {summary.pass}/
            {summary.total}
          </span>
        </>
      )}
    </div>
  );
}
