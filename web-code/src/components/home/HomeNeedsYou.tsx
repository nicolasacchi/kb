// OL-C7 — "Needs you": only the inbox lanes that are non-empty. When every
// lane is empty it is one quiet line, not four captions. Reuses
// `useUnifiedInbox` (same query as `~inbox`, so clicking through is a cache
// hit). Counts and links only; the real rows live in `routes/Inbox.tsx`.

import { StateCard } from "@kb/ui/StateCard";
import { Link } from "react-router";
import type { KbBlockedOut } from "../../api/types";
import { useKbBlocked } from "../../hooks/useKbBlocked";
import { useUnifiedInbox } from "../../hooks/useUnifiedInbox";
import { computeInboxBadge } from "../../lib/unifiedInbox";

export interface InboxLaneRow {
  lane: string;
  count: number;
  label: string;
  /// External link target (kb's Sessions page); absent rows link to ~inbox.
  href?: string;
}

/// OL IDEA1 - the "N sessions blocked on you" line, or null when kb is
/// unavailable or nothing is blocked. Pure. The href needs a configured
/// public url (never the internal federation url).
export function blockedLane(out?: KbBlockedOut): InboxLaneRow | null {
  if (!out || out.unavailable || out.sessions.length === 0) return null;
  const n = out.total > 0 ? out.total : out.sessions.length;
  let label: string;
  if (n === 1) {
    const s = out.sessions[0];
    label = `1 session blocked on you \u00b7 ${s.project ?? s.harness}`;
    if (s.blocked_tool) label += ` \u00b7 permission: ${s.blocked_tool}`;
  } else {
    label = `${n} sessions blocked on you`;
  }
  const base = out.kb_public_url.replace(/\/+$/, "");
  const row: InboxLaneRow = { lane: "kb-blocked", count: n, label };
  if (base) row.href = `${base}/sessions#kb-now-blocked`;
  return row;
}

/// The section badge: the inbox badge plus the blocked count.
export function needsYouBadge(inboxBadge: number | null, blocked: number): number {
  return (inboxBadge ?? 0) + blocked;
}

/// The non-empty lanes, in display order. Pure: the lane list is the only
/// thing Home's "Needs you" decides.
export function nonEmptyLanes(
  reviews: number,
  annotations: number,
  kbDesk: number | null,
  kbComments: number | null,
  blocked: InboxLaneRow | null = null,
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
  const kept = rows.filter((r) => r.count > 0);
  return blocked ? [blocked, ...kept] : kept;
}

export default function HomeNeedsYou() {
  const inbox = useUnifiedInbox();
  const data = inbox.data;
  const badge = data
    ? computeInboxBadge({ reviews: data.reviews, annotations: data.annotations, kb: data.kb })
    : null;
  const blocked = blockedLane(useKbBlocked().data);
  const rows = data
    ? nonEmptyLanes(
        data.reviews.length,
        data.annotations.length,
        data.kb.available && data.kb.desk ? data.kb.desk.attention : null,
        data.kb.available && data.kb.comments ? data.kb.comments.total_open : null,
        blocked,
      )
    : blocked
      ? [blocked]
      : [];
  const badgeN = needsYouBadge(badge, blocked?.count ?? 0);
  // While the inbox is loading or failed, only the kb-blocked line (which
  // does not depend on it) may stand above the status card.
  const listRows = inbox.isLoading || inbox.error ? (blocked ? [blocked] : []) : rows;

  return (
    <section className="kbc-home__section" aria-label="Inbox summary" data-kbc-home-inbox-summary>
      <h2 className="kbc-home__label">
        <Link to="/~inbox">Needs you</Link>
        {badgeN > 0 && (
          <span className="kbc-home__section-badge" data-kbc-home-inbox-summary-badge>
            {badgeN}
          </span>
        )}
      </h2>
      {listRows.length > 0 && (
        <ul className="kbc-home__needs" data-kbc-home-inbox-lanes>
          {listRows.map((r) => (
            <li key={r.lane}>
              {r.lane === "kb-blocked" ? (
                r.href ? (
                  <a
                    href={r.href}
                    target="_blank"
                    rel="noreferrer"
                    className="kbc-home__inbox-lane-link"
                    data-kbc-home-inbox-lane="kb-blocked"
                  >
                    {r.label}
                  </a>
                ) : (
                  <span className="kbc-home__inbox-lane-link" data-kbc-home-inbox-lane="kb-blocked">
                    {r.label}
                  </span>
                )
              ) : (
                <Link to="/~inbox" className="kbc-home__inbox-lane-link" data-kbc-home-inbox-lane={r.lane}>
                  {r.label}
                </Link>
              )}
            </li>
          ))}
        </ul>
      )}
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
      ) : null}
    </section>
  );
}
