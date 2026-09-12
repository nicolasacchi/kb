import { useMemo, useState } from "react";
import { Link, useNavigate, useParams } from "react-router";
import type { ReviewSummary, ReviewSummaryPr } from "../api/types";
import EmptyState from "../components/EmptyState";
import { Icon } from "../components/icons";
import AnalyticsSection from "../components/reviews/AnalyticsSection";
import InboxList from "../components/reviews/InboxList";
import StartReviewDialog from "../components/reviews/StartReviewDialog";
import UnreviewedPrsStrip from "../components/reviews/UnreviewedPrsStrip";
import { VerdictChip } from "../components/reviews/VerdictBar";
import { useReviewInbox, useReviews } from "../hooks/useReviews";
import { usePrs } from "../hooks/usePrs";
import { reviewUrl, prsUrl } from "../lib/codeUrl";
import { relativeTime } from "../lib/format";
import { joinInboxRows, unreviewedPrs } from "../lib/reviewInbox";
import "../styles/reviews.css";
import "../styles/review-inbox.css";
// PRR-U8 — `AnalyticsSection`'s `.kbc-stats` tile grid is defined in
// `review-room.css` (the cockpit's own stylesheet); the landing route
// pulls it in here rather than duplicating the rule set.
import "../styles/review-room.css";
import "../styles/review-analytics.css";
import { useListScrollRestoration } from "../hooks/useScrollRestoration";

/// V3.R2 → PRR-U1 (kb v0.39 "The PR Room," design-ui.md §2 S1) — `/r/{repo}/
/// ~reviews`: the Review Room landing. Four tiers, top to bottom:
///  1. INBOX — attention-ranked (server order from `GET /api/reviews/inbox`,
///     never re-sorted client-side), joined against the full reviews list
///     for `pr_meta`/`has_report`/`report_risk_score` (R4's additive
///     fields — see `lib/reviewInbox.ts`'s module doc for why the join is
///     client-side rather than a second live route).
///  2. Unreviewed PRs — `GET /api/prs` rows no review binds yet (deliberately
///     NOT part of the inbox route, which is local-only by construction —
///     see `review_inbox.rs`'s own doc on `pr_head_drift`).
///  3. All review sessions — today's table, demoted into a `<details>`
///     (`BrowseAllBranches` precedent), "Show closed" filter moved inside.
///  4. Analytics (PRR-U8, design-addendum-2.md §C) — the disposition
///     calibration instrument, a SECOND collapsed `<details>` below the
///     sessions fold (`components/reviews/AnalyticsSection.tsx`).
/// Fresh via `review.changed`/SSE invalidation on every query above, not
/// polling (the PR queries are the one documented finite-staleness
/// exception, same as `usePrs` always has been).
export default function Reviews() {
  // V70-A6 — root CLAUDE.md #31, ported: this list scrolls the WINDOW, so
  // one offset keyed on the full URL is the whole story. Back onto it lands
  // where the reader left, not where the browser guessed.
  useListScrollRestoration();
  const { repo = "" } = useParams<{ repo: string }>();
  const navigate = useNavigate();
  const [showClosed, setShowClosed] = useState(false);
  const [startOpen, setStartOpen] = useState(false);

  // The demoted table respects the "Show closed" toggle; the ALL-state list
  // below is a separate query (its own cache key) used purely for joins —
  // it must never miss a closed-but-PR-bound review, regardless of the
  // toggle (RankedBranchList precedent: its own `useReviews(repo, "open")`
  // is likewise independent of this page's toggle).
  const stateFilter = showClosed ? null : ("open" as const);
  const reviews = useReviews(repo, stateFilter);
  const list = reviews.data?.reviews ?? [];

  const allReviewsQ = useReviews(repo, null);
  // `ReviewSummary` doesn't declare R4's additive fields — the wire DOES
  // carry them (`pr_binding_and_report_fields`'s splice); intersect at the
  // call site, same convention `ReviewDetailPr` already established.
  const allReviews = (allReviewsQ.data?.reviews ?? []) as ReviewSummaryPr[];
  const openCount = useMemo(() => allReviews.filter((r) => r.state === "open").length, [allReviews]);

  const inboxQ = useReviewInbox(repo, "open");
  const inboxRows = useMemo(
    () => joinInboxRows(inboxQ.data?.reviews ?? [], allReviews),
    [inboxQ.data, allReviews],
  );

  const prsQ = usePrs(repo);
  const unreviewed = useMemo(
    () => unreviewedPrs(prsQ.data?.prs ?? [], allReviews),
    [prsQ.data, allReviews],
  );

  return (
    <div className="kbc-reviews" id="main">
      <header className="kbc-reviews__head">
        <div className="kbc-reviews__head-row">
          <h1 className="kbc-reviews__title">Review Room — {repo}</h1>
          <div className="kbc-reviews__head-actions">
            <Link to={prsUrl(repo)} className="kbc-reviews__all-prs" data-kbc-reviews-all-prs>
              all PRs →
            </Link>
            <button
              type="button"
              className="kbc-reviews__start"
              onClick={() => setStartOpen(true)}
              data-kbc-reviews-start
            >
              Start review
            </button>
          </div>
        </div>
        <p className="kbc-reviews__hint">
          Local Gerrit-lite review sessions — patchset snapshots, viewed-file tracking, annotations.
        </p>
      </header>

      <section className="kbc-inbox" data-kbc-inbox>
        <h2 className="kbc-inbox__title">Inbox — needs a human</h2>
        {inboxQ.isLoading ? (
          <div className="kbc-reader__hint">Loading inbox…</div>
        ) : inboxQ.error ? (
          <div className="kbc-reader__hint kbc-reader__hint--error">{(inboxQ.error as Error).message}</div>
        ) : inboxRows.length === 0 ? (
          <EmptyState
            icon={<Icon.ClipboardCheck />}
            title="Nothing needs you"
            hint={`${openCount} open session${openCount === 1 ? "" : "s"} below.`}
            variant="inline"
          />
        ) : (
          <InboxList repo={repo} rows={inboxRows} />
        )}
      </section>

      <UnreviewedPrsStrip repo={repo} prs={unreviewed} unavailableReason={prsQ.data?.unavailable_reason} />

      <details className="kbc-inbox-browse" data-kbc-browse open={list.length <= 5}>
        <summary className="kbc-inbox-browse__summary" data-kbc-browse-toggle>
          All review sessions ({list.length})
        </summary>
        <label className="kbc-reviews__filter">
          <input
            type="checkbox"
            checked={showClosed}
            onChange={(e) => setShowClosed(e.target.checked)}
            data-kbc-reviews-show-closed
          />
          Show closed
        </label>

        {reviews.isLoading ? (
          <div className="kbc-reader__hint">Loading reviews…</div>
        ) : reviews.error ? (
          <div className="kbc-reader__hint kbc-reader__hint--error">{(reviews.error as Error).message}</div>
        ) : list.length === 0 ? (
          <EmptyState
            icon={<Icon.List />}
            title={showClosed ? "No reviews yet" : "No open reviews"}
            hint='Click "Start review" to open a local review session against a branch tip.'
          />
        ) : (
          <ul className="kbc-reviews__list" data-kbc-reviews-list>
            {list.map((r) => (
              <ReviewRow key={r.id} repo={repo} review={r} />
            ))}
          </ul>
        )}
      </details>

      <AnalyticsSection repo={repo} />

      {startOpen && (
        <StartReviewDialog
          repo={repo}
          onClose={() => setStartOpen(false)}
          onCreated={(id) => {
            setStartOpen(false);
            navigate(reviewUrl(repo, id));
          }}
        />
      )}
    </div>
  );
}

function ReviewRow({ repo, review }: { repo: string; review: ReviewSummary }) {
  const title = review.title?.trim() || review.head_ref;
  const files = review.files_count;
  const viewed = review.viewed_count;
  const pct = files > 0 ? Math.round((viewed / files) * 100) : 0;

  return (
    <li className="kbc-reviews__row" data-kbc-reviews-row={review.id} data-kbc-reviews-state={review.state}>
      <Link to={reviewUrl(repo, review.id)} className="kbc-reviews__row-title" data-kbc-reviews-row-link={review.id}>
        {title}
      </Link>
      <span className="kbc-reviews__row-refs">
        <code>{review.head_ref}</code>
        <span aria-hidden="true">→</span>
        <code>{review.base_ref}</code>
      </span>
      <span className="kbc-reviews__row-meta">
        {review.latest_ps != null ? `ps${review.latest_ps}` : "no ps"}
        {review.state === "closed" && <span className="kbc-reviews__badge kbc-reviews__badge--closed">closed</span>}
        {review.verdict && (
          <span
            data-kbc-reviews-verdict={review.verdict.state}
            data-kbc-reviews-verdict-stale={review.verdict_stale ? "true" : undefined}
          >
            <VerdictChip
              verdict={review.verdict}
              stale={review.verdict_stale}
              latestPs={review.latest_ps}
            />
          </span>
        )}
      </span>
      <div
        className="kbc-reviews__progress"
        title={`${viewed}/${files} viewed`}
        data-kbc-reviews-progress={`${viewed}/${files}`}
      >
        <div className="kbc-reviews__progress-bar" style={{ width: `${pct}%` }} />
        <span className="kbc-reviews__progress-label">
          {viewed}/{files}
        </span>
      </div>
      {review.open_annotations > 0 && (
        <span className="kbc-reviews__ann-badge" data-kbc-reviews-ann={review.open_annotations}>
          {review.open_annotations} open
        </span>
      )}
      <span className="kbc-reviews__row-time" title={new Date(review.updated_at * 1000).toLocaleString()}>
        {relativeTime(review.updated_at)}
      </span>
    </li>
  );
}
