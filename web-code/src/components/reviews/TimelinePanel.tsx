// `review-timeline/2` (V73-K2c, design D9). Vertical feed, newest LAST
// (reading order = story order). V1 (PRR-U5+U6) fetched once with no
// filters and rendered the server's own stable-sorted sequence verbatim;
// V73-K2c widens this to the full stream — eleven lanes each reporting
// their own `ok`/`skipped`/`refused`/`degraded` state (never silently
// empty), server-side `kind`/`author`/`since`/`until` filters with true
// paging totals, and lane VISIBILITY as a client-side filter over the
// already-fetched page (the wire has no `?lane=` — `lib/timelineLanes.ts`
// owns that half).
//
// This file owns its OWN fetch now (previously `ReviewDetail.tsx` passed
// `data`/`loading`/`error` in) — the filter/paging/lane state lives here,
// where the controls that drive it render, following the same "a route
// mounts a self-fetching card" pattern `DiagnosticsCard`/`FrameworkCard`
// already establish elsewhere in this crate. `ReviewDetail.tsx` keeps its
// OWN unfiltered `useReviewTimeline` call for the tab-availability gate
// (`timelineAvailable`) and the 404→redirect toast — a second, cheap fetch
// is the accepted cost of not threading filter state through a route that
// has nothing else to do with it.
//
// V73-K2c also drops the v1 client-side GitHub-thread interleave
// (`githubTimelineRows`/`mergeTimelineRows`): `review-timeline/2`'s own
// `github` lane now natively carries `github_comment` events (LIVE, same
// `list_pull_comments` call), so merging a second client-built copy in
// would double the rows. `?github=` here maps straight to the server
// param.
//
// Purely a renderer over `lib/reviewTimeline.ts`'s `timelineRows` for the
// row SHAPE — this file never branches on a raw event `kind` itself, so an
// unrecognized future kind can't crash it.
import type { SVGProps } from "react";
import { useMemo, useState } from "react";
import type { ReviewTimelineLaneState } from "../../api/types";
import { useReviewTimeline } from "../../hooks/useReviews";
import { formatUnixSeconds, relativeTime } from "../../lib/format";
import { timelineRows, type TimelineIconKind } from "../../lib/reviewTimeline";
import { cycleLaneStep, laneLabel, TIMELINE_LANES, toggleLane } from "../../lib/timelineLanes";
import { Icon } from "../icons";
import ProseBlock from "../prose/ProseBlock";

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
  // V73-K2c — the five new server lanes' icons. No hand-drawn glyph exists
  // for any of these (`components/icons.tsx`'s own doc, PRR-F's identical
  // note on `github`) — each row's OWN kind/lane text + CSS modifier is
  // what actually disambiguates it, not the icon.
  pr_body: Icon.Note,
  wt_comment: Icon.Comment,
  doc_revision: Icon.Pen,
  report: Icon.ClipboardCheck,
  claim: Icon.Spark,
  turn: Icon.Terminal,
  github: Icon.PullRequest,
  unknown: Icon.More,
};

const LANE_STATE_TITLE: Record<ReviewTimelineLaneState, string> = {
  ok: "ok",
  skipped: "skipped",
  refused: "refused",
  degraded: "degraded",
};

const KIND_OPTIONS = [
  "review_created",
  "pr_bound",
  "patchset",
  "pr_body",
  "findings_import",
  "finding_added",
  "disposition",
  "verdict",
  "finding_published",
  "verdict_published",
  "comment",
  "wt_comment",
  "doc_revision",
  "report",
  "claim",
  "github_comment",
  "turn",
] as const;

function dateToUnix(d: string): number | undefined {
  if (!d) return undefined;
  const ms = Date.parse(`${d}T00:00:00Z`);
  return Number.isFinite(ms) ? Math.floor(ms / 1000) : undefined;
}

export interface TimelinePanelProps {
  repo: string;
  reviewId: number;
  /// Whether this review is PR-bound — governs the default state of the
  /// `github=0` toggle; the server itself already defaults `github` to "on
  /// for a PR-bound review, off otherwise", so this prop only steers the
  /// CHECKBOX's initial value, never the fetch itself.
  prBound?: boolean;
}

export default function TimelinePanel({ repo, reviewId, prBound }: TimelinePanelProps) {
  const [kind, setKind] = useState("");
  const [author, setAuthor] = useState("");
  const [since, setSince] = useState("");
  const [until, setUntil] = useState("");
  const [github, setGithub] = useState(!!prBound);
  const [limit, setLimit] = useState(100);
  const [hiddenLanes, setHiddenLanes] = useState<Set<string>>(new Set());
  const [laneCursor, setLaneCursor] = useState(0);

  const params = useMemo(
    () => ({
      kind: kind || undefined,
      author: author || undefined,
      since: dateToUnix(since),
      until: dateToUnix(until),
      github,
      limit,
    }),
    [kind, author, since, until, github, limit],
  );

  const q = useReviewTimeline(repo, reviewId, true, params);
  const data = q.data;

  const rows = useMemo(() => {
    if (!data) return [];
    return timelineRows(data.events, repo, reviewId);
  }, [data, repo, reviewId]);

  const visibleRows = useMemo(
    () => rows.filter((_, i) => !hiddenLanes.has(String(data?.events[i]?.lane ?? ""))),
    [rows, hiddenLanes, data],
  );

  function cycleLane() {
    const step = cycleLaneStep(hiddenLanes, laneCursor);
    setHiddenLanes(step.hidden);
    setLaneCursor(step.cursor);
  }

  if (q.isLoading) {
    return (
      <div className="kbc-timeline" data-kbc-timeline-loading>
        <div className="kbc-skeleton" style={{ height: 24, marginBottom: 10 }} />
        <div className="kbc-skeleton" style={{ height: 24, marginBottom: 10 }} />
        <div className="kbc-skeleton" style={{ height: 24 }} />
      </div>
    );
  }
  if (q.error) {
    return (
      <div className="kbc-reader__hint kbc-reader__hint--error" data-kbc-timeline-error>
        {(q.error as Error).message}
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
      {/* ── the lane bar — every one of the eleven lanes, always rendered
          with its own status chip. A lane hidden by the reader is still
          listed here (dimmed), never removed from view entirely — hiding
          the STATUS would be the exact "quietly lying about what was
          skipped" failure the wire exists to prevent. */}
      <div className="kbc-timeline__lanes" data-kbc-timeline-lanes role="group" aria-label="timeline lanes">
        {TIMELINE_LANES.map((lane) => {
          const status = data.sources.find((s) => s.lane === lane);
          const hidden = hiddenLanes.has(lane);
          return (
            <button
              key={lane}
              type="button"
              className={
                "kbc-timeline__lane" +
                (hidden ? " kbc-timeline__lane--hidden" : "") +
                (status ? ` kbc-timeline__lane--${status.state}` : "")
              }
              onClick={() => setHiddenLanes((prev) => toggleLane(prev, lane))}
              title={
                status
                  ? `${laneLabel(lane)}: ${LANE_STATE_TITLE[status.state]}${
                      status.reason ? ` — ${status.reason}` : ""
                    } (${status.count})`
                  : laneLabel(lane)
              }
              data-kbc-timeline-lane={lane}
              data-kbc-timeline-lane-state={status?.state ?? "skipped"}
              data-kbc-timeline-lane-hidden={hidden || undefined}
              aria-pressed={!hidden}
            >
              {laneLabel(lane)}
              {status && status.count > 0 && (
                <span className="kbc-timeline__lane-count">{status.count}</span>
              )}
            </button>
          );
        })}
        <button
          type="button"
          className="kbc-timeline__lane-cycle"
          onClick={cycleLane}
          title="cycle the next lane's visibility"
          data-kbc-timeline-lane-cycle
        >
          cycle lanes
        </button>
      </div>

      {/* ── filters — round-trip to the server (`kind`/`author`/`since`/
          `until`/`github`); lane visibility above does NOT round-trip. */}
      <div className="kbc-timeline__filters" data-kbc-timeline-filters>
        <select
          value={kind}
          onChange={(e) => setKind(e.target.value)}
          aria-label="filter by kind"
          data-kbc-timeline-filter-kind
        >
          <option value="">all kinds</option>
          {KIND_OPTIONS.map((k) => (
            <option key={k} value={k}>
              {k}
            </option>
          ))}
        </select>
        <input
          type="text"
          placeholder="author (human/agent/system or a name)"
          value={author}
          onChange={(e) => setAuthor(e.target.value)}
          aria-label="filter by author"
          data-kbc-timeline-filter-author
        />
        <input
          type="date"
          value={since}
          onChange={(e) => setSince(e.target.value)}
          aria-label="since"
          data-kbc-timeline-filter-since
        />
        <input
          type="date"
          value={until}
          onChange={(e) => setUntil(e.target.value)}
          aria-label="until"
          data-kbc-timeline-filter-until
        />
        <label className="kbc-timeline__github-toggle">
          <input
            type="checkbox"
            checked={github}
            onChange={(e) => setGithub(e.target.checked)}
            data-kbc-timeline-github-toggle
          />
          GitHub (live)
        </label>
      </div>

      {visibleRows.length === 0 ? (
        <p className="kbc-review__card-empty" data-kbc-timeline-empty>
          {rows.length === 0
            ? "Nothing has happened on this review yet."
            : "Every matching event is on a hidden lane."}
        </p>
      ) : (
        <ol className="kbc-timeline__spine">
          {visibleRows.map((row, i) => {
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
                        title="open externally"
                        data-kbc-timeline-external={row.kind}
                      >
                        <Icon.External />
                      </a>
                    )}
                  </div>
                  {row.author && (
                    <div className="kbc-timeline__author" data-kbc-timeline-author={row.author.kind}>
                      {row.author.kind}
                      {row.author.name ? ` · ${row.author.name}` : ""}
                      {row.author.model ? ` · ${row.author.model}` : ""}
                    </div>
                  )}
                  {row.detail && (
                    <div className="kbc-timeline__detail">
                      <ProseBlock text={row.detail} repo={repo} reviewId={reviewId} />
                    </div>
                  )}
                  {row.bodyMd && (
                    <div className="kbc-timeline__prose" data-kbc-timeline-body>
                      <ProseBlock text={row.bodyMd} refs={row.bodyRefs} repo={repo} reviewId={reviewId} />
                    </div>
                  )}
                  {row.driftNote && (
                    <div className="kbc-timeline__drift" data-kbc-timeline-drift title={row.driftNote}>
                      ⓘ {row.driftNote}
                    </div>
                  )}
                  <div className="kbc-timeline__at" title={formatUnixSeconds(row.at)}>
                    {relativeTime(row.at)}
                  </div>
                </div>
              </li>
            );
          })}
        </ol>
      )}

      {/* ── paging — TRUE totals, never a client-side count. */}
      <div className="kbc-timeline__paging" data-kbc-timeline-paging>
        <span data-kbc-timeline-paging-text>
          {data.returned} of {data.total} shown
        </span>
        {data.returned < data.total && (
          <button
            type="button"
            className="kbc-review__action"
            onClick={() => setLimit((l) => l + 100)}
            data-kbc-timeline-load-more
          >
            Load more
          </button>
        )}
      </div>
    </div>
  );
}
