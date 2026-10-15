// OL-C7 — "Needs you": only the inbox lanes that are non-empty. When every
// lane is empty it is one quiet line, not four captions. Reuses
// `useUnifiedInbox` (same query as `~inbox`, so clicking through is a cache
// hit). Counts and links only; the real rows live in `routes/Inbox.tsx`.

import { StateCard } from "@kb/ui/StateCard";
import { Link } from "react-router";
import { useUnifiedInbox } from "../../hooks/useUnifiedInbox";
import { computeInboxBadge } from "../../lib/unifiedInbox";

export interface InboxLaneRow {
  lane: string;
  count: number;
  label: string;
}

/// The non-empty lanes, in display order. Pure: the lane list is the only
/// thing Home's "Needs you" decides.
export function nonEmptyLanes(
  reviews: number,
  annotations: number,
  kbDesk: number | null,
  kbComments: number | null,
): InboxLaneRow[] {
  const plural = (n: number, one: string, many: string) => `${n} ${n === 1 ? one : many}`;
  const rows: InboxLaneRow[] = [
    { lane: "reviews", count: reviews, label: `${plural(reviews, "review", "reviews")} awaiting you` },
    {
      lane: "annotations",
      count: annotations,
      label: `${plural(annotations, "open question", "open questions")} in working trees`,
    },
  ];
  if (kbDesk !== null) rows.push({ lane: "kb-desk", count: kbDesk, label: `${kbDesk} on your kb desk` });
  if (kbComments !== null) {
    rows.push({
      lane: "kb-comments",
      count: kbComments,
      label: `${plural(kbComments, "open kb comment", "open kb comments")}`,
    });
  }
  return rows.filter((r) => r.count > 0);
}

export default function HomeNeedsYou() {
  const inbox = useUnifiedInbox();
  const data = inbox.data;
  const badge = data
    ? computeInboxBadge({ reviews: data.reviews, annotations: data.annotations, kb: data.kb })
    : null;
  const rows = data
    ? nonEmptyLanes(
        data.reviews.length,
        data.annotations.length,
        data.kb.available && data.kb.desk ? data.kb.desk.attention : null,
        data.kb.available && data.kb.comments ? data.kb.comments.total_open : null,
      )
    : [];

  return (
    <section className="kbc-home__section" aria-label="Inbox summary" data-kbc-home-inbox-summary>
      <h2 className="kbc-home__label">
        <Link to="/~inbox">Needs you</Link>
        {badge !== null && badge > 0 && (
          <span className="kbc-home__section-badge" data-kbc-home-inbox-summary-badge>
            {badge}
          </span>
        )}
      </h2>
      {inbox.isLoading ? (
        <StateCard kind="loading" title="Loading inbox…" />
      ) : inbox.error ? (
        <StateCard
          kind="degraded"
          title="Couldn't load the inbox."
          reason={(inbox.error as Error).message}
          headingLevel={3}
        />
      ) : data && rows.length === 0 ? (
        <p className="kbc-home__quiet" data-kbc-home-inbox-empty>
          Nothing needs you
        </p>
      ) : data ? (
        <ul className="kbc-home__needs" data-kbc-home-inbox-lanes>
          {rows.map((r) => (
            <li key={r.lane}>
              <Link to="/~inbox" className="kbc-home__inbox-lane-link" data-kbc-home-inbox-lane={r.lane}>
                {r.label}
              </Link>
            </li>
          ))}
        </ul>
      ) : null}
    </section>
  );
}
