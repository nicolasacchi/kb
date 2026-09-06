// PRR-F — recurring-finding memory (design-ui.md §12.4, frontier 4). A
// quiet chip on a finding whose `(category, location_path)` pair recurred
// across `>= store::RECURRENCE_MIN_REVIEWS` distinct reviews (`GET
// /api/reviews/{id}/findings/recurrence` — the SAME shared recurrence query
// `GET /api/reviews/analytics`'s own `recurrence` field uses server-side).
// Click opens a small popover listing the prior reviews as links. Renders
// `null` for a non-recurring finding — named absence, not an empty chip
// (same "no fabricated zero" posture every other chip in this unit uses).
//
// Multiple mounted chips on one review all call `useFindingsRecurrence`
// with the SAME query key — TanStack Query dedupes to one request/cache
// entry (the same "shared fetch across mounts" precedent `ReviewSidePanel`'s
// own doc documents for `useReviewComments`/`useReviewFindings`).
import { useState } from "react";
import { useFindingsRecurrence } from "../../hooks/useReviews";
import { reviewUrl } from "../../lib/codeUrl";
import { relativeTime } from "../../lib/format";

export interface RecurrenceChipProps {
  repo: string;
  reviewId: number;
  slug: string;
}

export default function RecurrenceChip({ repo, reviewId, slug }: RecurrenceChipProps) {
  const q = useFindingsRecurrence(repo, reviewId);
  const [open, setOpen] = useState(false);
  const row = q.data?.findings.find((f) => f.slug === slug);
  if (!row) return null;

  return (
    <span className="kbc-finding__recur" data-kbc-finding-recurrence={slug}>
      <button
        type="button"
        className="kbc-finding__recur-toggle"
        onClick={(e) => {
          e.preventDefault();
          e.stopPropagation();
          setOpen((v) => !v);
        }}
        title={`Seen in ${row.seen_in_reviews} reviews — click for prior reviews`}
        aria-expanded={open}
        data-kbc-finding-recurrence-toggle={slug}
      >
        seen in {row.seen_in_reviews} reviews
      </button>
      {open && (
        <div
          className="kbc-finding__recur-pop"
          role="dialog"
          aria-label="prior reviews"
          data-kbc-finding-recurrence-pop={slug}
        >
          {row.prior.length === 0 ? (
            <p className="kbc-review__card-empty">No other reviews currently on record.</p>
          ) : (
            <ul>
              {row.prior.map((p) => (
                <li key={p.review_id}>
                  <a
                    href={reviewUrl(repo, p.review_id)}
                    target="_blank"
                    rel="noreferrer"
                    data-kbc-finding-recurrence-prior={p.review_id}
                  >
                    {p.pr_number != null ? `PR #${p.pr_number}` : `Review #${p.review_id}`}
                    {p.title ? ` — ${p.title}` : ""}
                  </a>
                  <span className="kbc-finding__recur-at"> · {relativeTime(p.created_at)}</span>
                </li>
              ))}
            </ul>
          )}
        </div>
      )}
    </span>
  );
}
