// PRR-U5+U6 (design-ui.md §2 S4 — Timeline tab, "SHOULD"). Vertical feed,
// newest LAST (reading order = story order), one `GET /api/reviews/{id}/
// timeline` fetch, no client-side sorting (the server already returns a
// stable-sorted-ascending sequence — `review_timeline.rs`'s own doc/tests).
// Purely a renderer over `lib/reviewTimeline.ts`'s `timelineRows` — this
// file never branches on a raw event `kind` itself, so an unrecognized
// future kind can't crash it (the pure mapper already degraded it to a
// plain-label row).
import type { SVGProps } from "react";
import { useMemo } from "react";
import type { GithubThreadsOut, ReviewTimelineOut } from "../../api/types";
import { formatUnixSeconds, relativeTime } from "../../lib/format";
import { githubTimelineRows, mergeTimelineRows } from "../../lib/githubThreads";
import { timelineRows, type TimelineIconKind } from "../../lib/reviewTimeline";
import { Icon } from "../icons";

const ICONS: Record<TimelineIconKind, (p: SVGProps<SVGSVGElement>) => JSX.Element> = {
  created: Icon.Note,
  pr: Icon.PullRequest,
  patchset: Icon.Layers,
  import: Icon.ClipboardCheck,
  finding: Icon.Plus,
  disposition: Icon.Check,
  verdict: Icon.Flame,
  published: Icon.External,
  comment: Icon.Comment,
  // PRR-F — no GitHub brand glyph exists in this crate's hand-drawn icon
  // set (`components/icons.tsx`'s own doc) — `Icon.PullRequest` is the
  // closest existing PR-shaped glyph; the row's own "distinct row style"
  // comes from the `kbc-timeline__glyph--github` class + text, not a new
  // invented icon.
  github: Icon.PullRequest,
  unknown: Icon.More,
};

export interface TimelinePanelProps {
  repo: string;
  reviewId: number;
  loading: boolean;
  error: Error | null;
  data: ReviewTimelineOut | null | undefined;
  /// PRR-F (design-addendum-2.md §A) — client-side interleave only; the
  /// server timeline route doesn't know GitHub at all. `undefined`/`null`
  /// (no PR binding, older server, still loading) degrades to server rows
  /// only — never blocks the panel.
  githubThreads?: GithubThreadsOut | null;
}

export default function TimelinePanel({
  repo,
  reviewId,
  loading,
  error,
  data,
  githubThreads,
}: TimelinePanelProps) {
  const rows = useMemo(() => {
    if (!data) return [];
    const serverRows = timelineRows(data.events, repo, reviewId);
    const ghRows = githubThreads ? githubTimelineRows(githubThreads.threads) : [];
    return ghRows.length > 0 ? mergeTimelineRows(serverRows, ghRows) : serverRows;
  }, [data, repo, reviewId, githubThreads]);

  if (loading) {
    return (
      <div className="kbc-timeline" data-kbc-timeline-loading>
        <div className="kbc-skeleton" style={{ height: 24, marginBottom: 10 }} />
        <div className="kbc-skeleton" style={{ height: 24, marginBottom: 10 }} />
        <div className="kbc-skeleton" style={{ height: 24 }} />
      </div>
    );
  }
  if (error) {
    return (
      <div className="kbc-reader__hint kbc-reader__hint--error" data-kbc-timeline-error>
        {error.message}
      </div>
    );
  }
  if (!data) {
    return (
      <div className="kbc-reader__hint" data-kbc-timeline-absent>
        Timeline is not available on this server version.
      </div>
    );
  }

  return (
    <div className="kbc-timeline" data-kbc-timeline={reviewId}>
      {rows.length === 0 ? (
        <p className="kbc-review__card-empty" data-kbc-timeline-empty>
          Nothing has happened on this review yet.
        </p>
      ) : (
        <ol className="kbc-timeline__spine">
          {rows.map((row, i) => {
            const IconCmp = ICONS[row.icon];
            return (
              <li
                key={`${row.kind}-${row.at}-${i}`}
                className="kbc-timeline__row"
                data-kbc-timeline-row={row.kind}
              >
                <span className={`kbc-timeline__glyph kbc-timeline__glyph--${row.icon}`} aria-hidden="true">
                  <IconCmp />
                </span>
                <div className="kbc-timeline__body">
                  <div className="kbc-timeline__label">
                    {row.href ? (
                      <a href={row.href} data-kbc-timeline-link={row.kind}>
                        {row.label}
                      </a>
                    ) : (
                      row.label
                    )}
                    {row.external && (
                      <a
                        className="kbc-timeline__external"
                        href={row.external}
                        target="_blank"
                        rel="noreferrer"
                        title="open on GitHub"
                        data-kbc-timeline-external={row.kind}
                      >
                        <Icon.External />
                      </a>
                    )}
                  </div>
                  {row.detail && <div className="kbc-timeline__detail">{row.detail}</div>}
                  <div className="kbc-timeline__at" title={formatUnixSeconds(row.at)}>
                    {relativeTime(row.at)}
                  </div>
                </div>
              </li>
            );
          })}
        </ol>
      )}
    </div>
  );
}
