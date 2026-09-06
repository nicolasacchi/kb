// S2-A — kb-code v6.0 "One Inbox" (design-s2.md §S2-A). Top-level,
// repo-less page: `GET /api/inbox`'s three lanes, each keeping its own
// source ordering (never a merged cross-lane score — see
// `api/types.ts`'s `UnifiedInboxOut` doc). Finite staleTime + manual
// Refresh (`hooks/useUnifiedInbox.ts`'s own doc has the full no-SSE-tie
// rationale) rather than a loading spinner on every mount.
import { Link } from "react-router-dom";
import type { UnifiedInboxAnnotationRow } from "../api/types";
import EmptyState from "../components/EmptyState";
import { Icon } from "../components/icons";
import InboxList from "../components/reviews/InboxList";
import { useIdentity } from "../hooks/useIdentity";
import { useUnifiedInbox } from "../hooks/useUnifiedInbox";
import {
  annotationReaderUrl,
  computeInboxBadge,
  groupByRepo,
  kbArtifactUrl,
  kbCommentUrl,
  kbLaneState,
  truncationCaption,
} from "../lib/unifiedInbox";
import { joinInboxRows } from "../lib/reviewInbox";
import { relativeTime } from "../lib/format";
import "../styles/review-inbox.css";
import "../styles/inbox.css";
import { useListScrollRestoration } from "../hooks/useScrollRestoration";

export default function Inbox() {
  // V70-A6 — root CLAUDE.md #31, ported: this list scrolls the WINDOW, so
  // one offset keyed on the full URL is the whole story. Back onto it lands
  // where the reader left, not where the browser guessed.
  useListScrollRestoration();
  const identity = useIdentity();
  const inbox = useUnifiedInbox();
  const data = inbox.data;

  // No per-repo `ReviewSummaryPr` join available here (that would mean
  // fan-out fetches across every configured repo just to color a dot) —
  // `joinInboxRows(rows, [])` is the SAME safe degrade `lib/reviewInbox.ts`
  // already documents for a missing join: every row still renders, just
  // `hollow`/`riskScore: null` instead of a colored risk dot.
  const reviewRows = joinInboxRows(data?.reviews ?? [], []);
  const reviewGroups = groupByRepo(reviewRows);
  const badge = data
    ? computeInboxBadge({ reviews: data.reviews, annotations: data.annotations, kb: data.kb })
    : null;

  return (
    <div className="kbc-inboxpage" id="main">
      <header className="kbc-inboxpage__head">
        <h1 className="kbc-inboxpage__title">
          Inbox
          {badge !== null && badge > 0 && <span className="kbc-inboxpage__badge" data-kbc-inbox-badge>{badge}</span>}
        </h1>
        <button
          type="button"
          className="kbc-inboxpage__refresh"
          onClick={() => void inbox.refetch()}
          disabled={inbox.isFetching}
          data-kbc-inbox-refresh
        >
          <Icon.Refresh width={12} height={12} />
          {inbox.isFetching ? "Refreshing…" : "Refresh"}
        </button>
      </header>

      {inbox.isLoading ? (
        <div className="kbc-reader__hint">Loading inbox…</div>
      ) : inbox.error ? (
        <div className="kbc-reader__hint kbc-reader__hint--error">{(inbox.error as Error).message}</div>
      ) : !data ? null : (
        <>
          <section className="kbc-inbox" data-kbc-inbox-reviews>
            <h2 className="kbc-inbox__title">Reviews awaiting you</h2>
            {reviewRows.length === 0 ? (
              <EmptyState icon={<Icon.ClipboardCheck />} title="Nothing needs you" variant="inline" />
            ) : (
              reviewGroups.map((g) => (
                <div key={g.repo} className="kbc-inboxpage__repo-group" data-kbc-inbox-repo-group={g.repo}>
                  <h3 className="kbc-inboxpage__repo-label">{g.repo}</h3>
                  <InboxList repo={g.repo} rows={g.rows} />
                </div>
              ))
            )}
          </section>

          <section className="kbc-inbox" data-kbc-inbox-annotations>
            <h2 className="kbc-inbox__title">Open questions in working trees</h2>
            {data.annotations.length === 0 ? (
              <EmptyState icon={<Icon.Comment />} title="No open questions" variant="inline" />
            ) : (
              <ul className="kbc-inboxpage__ann-list" data-kbc-inbox-ann-list>
                {data.annotations.map((row) => (
                  <AnnotationRow key={row.id} row={row} />
                ))}
              </ul>
            )}
          </section>

          <KbSection kbBase={identity.data?.kb_public_url} kb={data.kb} />
        </>
      )}
    </div>
  );
}

function AnnotationRow({ row }: { row: UnifiedInboxAnnotationRow }) {
  return (
    <li className="kbc-inboxpage__ann-row" data-kbc-inbox-ann-row={row.id}>
      <Link to={annotationReaderUrl(row)} className="kbc-inboxpage__ann-link" data-kbc-inbox-ann-link={row.id}>
        <span className="kbc-inboxpage__ann-intent" data-kbc-inbox-ann-intent={row.intent}>
          {row.intent}
        </span>
        <span className="kbc-inboxpage__ann-loc">
          {row.repo}
          <span aria-hidden="true">·</span>
          {row.path}
          {row.line != null && <span aria-hidden="true">:{row.line}</span>}
        </span>
        <span className="kbc-inboxpage__ann-excerpt">{row.excerpt}</span>
        <span className="kbc-inboxpage__ann-meta">
          {row.author}
          {row.reply_count > 0 && (
            <span>
              {row.reply_count} repl{row.reply_count === 1 ? "y" : "ies"}
            </span>
          )}
          <span title={new Date(row.updated_at * 1000).toLocaleString()}>{relativeTime(row.updated_at)}</span>
        </span>
      </Link>
    </li>
  );
}

function KbSection({
  kbBase,
  kb,
}: {
  kbBase: string | undefined;
  kb: NonNullable<ReturnType<typeof useUnifiedInbox>["data"]>["kb"];
}) {
  const state = kbLaneState(kb);
  // `useIdentity`'s own boot fetch feeds `kb_public_url`; an older/absent
  // daemon leaves it `undefined` — fall back to kb-code's documented
  // local-dev default rather than emitting `href="undefined/a/..."`
  // (`lib/searchLanes.ts`'s `DEFAULT_KB_SESSION_BASE` precedent, kept as a
  // local literal here since that module's own base is private state, not
  // an export).
  const base = kbBase ?? "http://127.0.0.1:4000";

  return (
    <section className="kbc-inbox" data-kbc-inbox-kb>
      <h2 className="kbc-inbox__title">From kb</h2>
      {state.kind === "unavailable" ? (
        <p className="kbc-inboxpage__kb-reason" data-kbc-inbox-kb-reason={state.reason}>
          {state.label}
        </p>
      ) : (
        <>
          <KbDeskList base={base} desk={kb.desk} />
          <KbCommentsList base={base} comments={kb.comments} />
        </>
      )}
    </section>
  );
}

function KbDeskList({ base, desk }: { base: string; desk: NonNullable<ReturnType<typeof useUnifiedInbox>["data"]>["kb"]["desk"] }) {
  if (!desk || desk.items.length === 0) {
    return <EmptyState icon={<Icon.Note />} title="Desk is empty" variant="inline" />;
  }
  const caption = truncationCaption(desk.truncated);
  return (
    <div className="kbc-inboxpage__kb-sub" data-kbc-inbox-kb-desk>
      <h3 className="kbc-inboxpage__kb-sub-title">
        Desk
        <span className="kbc-inboxpage__kb-count">{desk.attention}</span>
      </h3>
      <ul className="kbc-inboxpage__kb-list" data-kbc-inbox-kb-desk-list>
        {desk.items.map((item, i) => {
          const kbName = typeof item.kb === "string" ? item.kb : "";
          const rel = typeof item.source_relative === "string" ? item.source_relative : null;
          const title = typeof item.title === "string" ? item.title : (rel ?? "(untitled)");
          const key = typeof item.id === "string" ? item.id : `${kbName}-${i}`;
          const href = kbName && rel ? kbArtifactUrl(base, kbName, rel) : null;
          return (
            <li key={key} className="kbc-inboxpage__kb-row" data-kbc-inbox-kb-desk-row={key}>
              {href ? (
                <a href={href} target="_blank" rel="noreferrer" data-kbc-inbox-kb-link={key}>
                  {title} <Icon.External width={11} height={11} aria-hidden />
                </a>
              ) : (
                <span>{title}</span>
              )}
              {typeof item.updated_unix === "number" && (
                <span title={new Date(item.updated_unix * 1000).toLocaleString()}>
                  {relativeTime(item.updated_unix)}
                </span>
              )}
              {typeof item.comments_open === "number" && item.comments_open > 0 && (
                <span>{item.comments_open} open</span>
              )}
            </li>
          );
        })}
      </ul>
      {caption && (
        <p className="kbc-inboxpage__kb-caption" data-kbc-inbox-kb-desk-truncated>
          {caption}
        </p>
      )}
    </div>
  );
}

function KbCommentsList({
  base,
  comments,
}: {
  base: string;
  comments: NonNullable<ReturnType<typeof useUnifiedInbox>["data"]>["kb"]["comments"];
}) {
  if (!comments || comments.items.length === 0) {
    return <EmptyState icon={<Icon.Comment />} title="No open comments" variant="inline" />;
  }
  const caption = truncationCaption(comments.truncated);
  return (
    <div className="kbc-inboxpage__kb-sub" data-kbc-inbox-kb-comments>
      <h3 className="kbc-inboxpage__kb-sub-title">
        Open comments
        <span className="kbc-inboxpage__kb-count">{comments.total_open}</span>
      </h3>
      <ul className="kbc-inboxpage__kb-list" data-kbc-inbox-kb-comments-list>
        {comments.items.map((item, i) => {
          const kbName = typeof item.kb === "string" ? item.kb : "";
          const rel = typeof item.source_relative === "string" ? item.source_relative : null;
          const title = typeof item.title === "string" ? item.title : (rel ?? "(untitled)");
          const key = typeof item.comment_id === "string" ? item.comment_id : `${kbName}-${i}`;
          const href = kbName && rel ? kbCommentUrl(base, kbName, rel) : null;
          return (
            <li key={key} className="kbc-inboxpage__kb-row" data-kbc-inbox-kb-comments-row={key}>
              {href ? (
                <a href={href} target="_blank" rel="noreferrer" data-kbc-inbox-kb-link={key}>
                  {title} <Icon.External width={11} height={11} aria-hidden />
                </a>
              ) : (
                <span>{title}</span>
              )}
              {typeof item.excerpt === "string" && item.excerpt && (
                <span className="kbc-inboxpage__kb-excerpt">{item.excerpt}</span>
              )}
              {typeof item.author === "string" && <span>{item.author}</span>}
              {typeof item.updated_at === "number" && (
                <span title={new Date(item.updated_at * 1000).toLocaleString()}>
                  {relativeTime(item.updated_at)}
                </span>
              )}
            </li>
          );
        })}
      </ul>
      {caption && (
        <p className="kbc-inboxpage__kb-caption" data-kbc-inbox-kb-comments-truncated>
          {caption}
        </p>
      )}
    </div>
  );
}
