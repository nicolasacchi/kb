// PRR-U1 — kb v0.39 "The PR Room," unit U1 (design-ui.md §2 S1): the
// landing page's attention-ranked inbox list. Pure render over
// `lib/reviewInbox.ts`'s `InboxViewRow[]` — server order is never re-sorted
// here (`review_inbox::sort_inbox_rows` already did the ranking).
//
// Deliberately does NOT show a live CI summary or the report's own authored
// verdict word — neither is on the inbox row (`review_inbox.rs`'s own doc:
// no per-row live GitHub call, no re-derived verdict enum); the risk-score
// bucket (`inboxDotTone`) and the row's own named terms (reason chips) are
// what's honestly available without an N-request storm on one page load.
import { Link } from "react-router-dom";
import type { InboxViewRow } from "../../lib/reviewInbox";
import { reviewUrl } from "../../lib/codeUrl";
import { relativeTime } from "../../lib/format";

export interface InboxListProps {
  repo: string;
  rows: InboxViewRow[];
}

export default function InboxList({ repo, rows }: InboxListProps) {
  return (
    <ul className="kbc-inbox__list" data-kbc-inbox-list>
      {rows.map((row) => (
        <InboxRow key={row.reviewId} repo={repo} row={row} />
      ))}
    </ul>
  );
}

function InboxRow({ repo, row }: { repo: string; row: InboxViewRow }) {
  return (
    <li>
      <Link
        to={reviewUrl(repo, row.reviewId)}
        className="kbc-inbox__row"
        data-kbc-inbox-row={row.reviewId}
        data-kbc-inbox-dot={row.dot}
      >
        <span
          className={`kbc-inbox__dot kbc-inbox__dot--${row.dot}`}
          aria-hidden="true"
          title={row.hasReport ? `risk ${row.riskScore ?? "?"}/10` : "no agent review yet"}
        />
        <span className="kbc-inbox__body">
          <span className="kbc-inbox__title-row">
            {row.prNumber != null && <span className="kbc-inbox__pr">#{row.prNumber}</span>}
            <span className="kbc-inbox__row-title">{row.title}</span>
          </span>
          <span className="kbc-inbox__meta">
            {row.prAuthor && <span>{row.prAuthor}</span>}
            {row.prDraft && <span>draft</span>}
            {row.hasReport && row.riskScore != null && (
              <span data-kbc-inbox-risk={row.riskScore}>risk {row.riskScore}/10</span>
            )}
            {!row.hasReport && <span>no agent review yet</span>}
            <span title={new Date(row.updatedAt * 1000).toLocaleString()}>{relativeTime(row.updatedAt)}</span>
          </span>
          {row.chips.length > 0 && (
            <span className="kbc-inbox__chips">
              {row.chips.map((c) => (
                <span key={c.key} className="kbc-inbox-chip" data-kbc-inbox-reason={c.key}>
                  {c.label}
                </span>
              ))}
            </span>
          )}
        </span>
        <span className="kbc-inbox__open" aria-hidden="true">
          Open room
        </span>
      </Link>
    </li>
  );
}
