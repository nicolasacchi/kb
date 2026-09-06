// PRR-U8 (design-addendum-2.md §C, kb v0.39 "The PR Room") — the Review
// Room landing's Analytics section: a collapsed `<details>` below the "All
// review sessions" fold (`routes/Reviews.tsx`'s own module doc numbers the
// landing's tiers; this is an ADDITIVE fourth). Stat tiles (findings total,
// acceptance by severity, median/p90 time-to-disposition) + a CSS-bar
// severity×disposition breakdown + the recurrence list — no chart library
// (addendum §C, verbatim). Every null (`rate`/`median_secs`/`p90_secs`)
// renders as an honest em-dash, never a fabricated `0`/`0%`
// (`lib/reviewAnalytics.ts`'s own doc + `review_analytics.rs`'s module
// doc — "no fabricated 0%" is the server's own law, this just doesn't
// betray it in the renderer).
//
// Lazy: the query only fires once the `<details>` is actually opened
// (`onToggle` flips local `open` state, threaded into `useReviewAnalytics`'s
// `enabled`) — a collapsed-by-default section that ALWAYS fetched on
// landing-page load would cost every `~reviews` visit a full corpus scan
// for a number most visits never look at.
import { useState } from "react";
import { Link } from "react-router-dom";
import type { ReviewAnalyticsOut } from "../../api/types";
import { useReviewAnalytics } from "../../hooks/useReviews";
import { reviewUrl } from "../../lib/codeUrl";
import {
  dispositionSegments,
  formatAnalyticsLatency,
  formatAnalyticsRate,
  groupBySeverity,
} from "../../lib/reviewAnalytics";
import "../../styles/review-analytics.css";

export interface AnalyticsSectionProps {
  repo: string;
}

export default function AnalyticsSection({ repo }: AnalyticsSectionProps) {
  const [open, setOpen] = useState(false);
  const q = useReviewAnalytics(repo, open);

  return (
    <details
      className="kbc-analytics"
      data-kbc-analytics
      onToggle={(e) => setOpen((e.target as HTMLDetailsElement).open)}
    >
      <summary className="kbc-analytics__summary" data-kbc-analytics-toggle>
        Analytics
      </summary>
      {!open ? null : q.isLoading ? (
        <div data-kbc-analytics-loading>
          <div className="kbc-skeleton" style={{ height: 72, marginBottom: 12 }} />
          <div className="kbc-skeleton" style={{ height: 120 }} />
        </div>
      ) : q.error ? (
        <div className="kbc-reader__hint kbc-reader__hint--error" data-kbc-analytics-error>
          {(q.error as Error).message}
        </div>
      ) : q.data ? (
        <AnalyticsBody repo={repo} data={q.data} />
      ) : null}
    </details>
  );
}

function AnalyticsBody({ repo, data }: { repo: string; data: ReviewAnalyticsOut }) {
  const grouped = groupBySeverity(data.by_severity_disposition);

  return (
    <div data-kbc-analytics-body>
      <div className="kbc-stats kbc-analytics__stats" data-kbc-analytics-stats>
        <div className="cell">
          <div className="l">Findings</div>
          <div className="v" data-kbc-analytics-stat="findings">
            {data.total_findings}
          </div>
          {data.superseded_count > 0 && (
            <div className="kbc-analytics__stat-caption" data-kbc-analytics-superseded>
              {data.superseded_count} superseded (excluded above)
            </div>
          )}
        </div>
        <div className="cell">
          <div className="l">Median time-to-disposition</div>
          <div className="v" data-kbc-analytics-stat="median-latency">
            {formatAnalyticsLatency(data.latency.median_secs)}
          </div>
        </div>
        <div className="cell">
          <div className="l">p90 time-to-disposition</div>
          <div className="v" data-kbc-analytics-stat="p90-latency">
            {formatAnalyticsLatency(data.latency.p90_secs)}
          </div>
        </div>
      </div>

      <div className="kbc-analytics__acceptance" data-kbc-analytics-acceptance>
        <div className="kbc-eyebrow">Acceptance by severity</div>
        {data.acceptance.map((a) => (
          <div
            key={a.severity}
            className="kbc-analytics__acc-row"
            data-kbc-analytics-acceptance-row={a.severity}
          >
            <span className="kbc-finding__sev" data-kbc-finding-severity={a.severity}>
              {a.severity}
            </span>
            <span className="kbc-analytics__acc-rate" data-kbc-analytics-acceptance-rate={a.severity}>
              {formatAnalyticsRate(a.rate)}
            </span>
            <span className="kbc-analytics__acc-detail">
              {a.accepted} accepted · {a.rejected} rejected · {a.risk_accepted} risk-accepted ·{" "}
              {a.undecided} undecided
            </span>
          </div>
        ))}
      </div>

      <div className="kbc-analytics__matrix" data-kbc-analytics-matrix>
        <div className="kbc-eyebrow">Severity × disposition</div>
        {[...grouped.entries()].map(([severity, cells]) => {
          const segs = dispositionSegments(cells);
          const rowTotal = cells.reduce((s, c) => s + c.count, 0);
          return (
            <div
              key={severity}
              className="kbc-analytics__matrix-row"
              data-kbc-analytics-matrix-row={severity}
            >
              <span className="kbc-analytics__matrix-label">{severity}</span>
              <div
                className="kbc-analytics__bar"
                role="img"
                aria-label={`${severity}: ${rowTotal} finding${rowTotal === 1 ? "" : "s"}`}
              >
                {segs.map(
                  (s) =>
                    s.count > 0 && (
                      <span
                        key={s.disposition}
                        className={`kbc-analytics__bar-seg kbc-analytics__bar-seg--${s.disposition}`}
                        style={{ width: `${s.pct}%` }}
                        title={`${s.disposition}: ${s.count}`}
                        data-kbc-analytics-bar-seg={`${severity}-${s.disposition}`}
                      />
                    ),
                )}
              </div>
              <span className="kbc-analytics__matrix-total">{rowTotal}</span>
            </div>
          );
        })}
      </div>

      <div className="kbc-analytics__recurrence" data-kbc-analytics-recurrence>
        <div className="kbc-eyebrow">Recurring findings</div>
        {data.recurrence.length === 0 ? (
          <p className="kbc-review__card-empty" data-kbc-analytics-recurrence-empty>
            No finding recurs across reviews yet.
          </p>
        ) : (
          <ul className="kbc-analytics__recurrence-list">
            {data.recurrence.map((r, i) => (
              <li key={`${r.category}::${r.location_path}`} data-kbc-analytics-recurrence-row={i}>
                <div className="kbc-analytics__recurrence-head">
                  <span className="kbc-analytics__recurrence-cat">{r.category}</span>
                  <code className="kbc-analytics__recurrence-loc">{r.location_path}</code>
                  <span className="kbc-analytics__recurrence-count">
                    seen in {r.review_count} review{r.review_count === 1 ? "" : "s"}
                  </span>
                </div>
                <div className="kbc-analytics__recurrence-reviews">
                  {r.review_ids.map((id) => (
                    <Link key={id} to={reviewUrl(repo, id)} data-kbc-analytics-recurrence-review={id}>
                      #{id}
                    </Link>
                  ))}
                </div>
              </li>
            ))}
          </ul>
        )}
      </div>
    </div>
  );
}
