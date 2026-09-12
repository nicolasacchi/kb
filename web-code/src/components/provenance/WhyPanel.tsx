import { useState } from "react";
import { Link } from "react-router";
import { Icon } from "../icons";
import { UNCOMMITTED_SHA, type BlameRegion, type LineWhyOut } from "../../api/types";
import { useBlameTimeline } from "../../hooks/useBlameTimeline";
import { useSessionDiff } from "../../hooks/useSessionDiff";
import { commitUrl } from "../../lib/codeUrl";
import { formatUnixSeconds, shortSha } from "../../lib/format";
import { sessionUrl } from "../../lib/searchLanes";
import { siblingFiles } from "../../lib/sessionDiff";
import OriginatingChange from "./OriginatingChange";

export interface WhyPanelProps {
  repo: string;
  /// The configured repo's absolute working-tree path (`GET /api/repos`'
  /// `RepoListEntry.path`) — relativizes the "sibling files" section's
  /// uncommitted-evidence paths (`lib/sessionDiff.ts`'s `siblingFiles`).
  repoRoot?: string;
  path: string;
  line: number;
  /// The blame region covering `line`, from the SAME `GET /api/blame`
  /// result the gutter renders from (`lib/blameGutter.ts`'s
  /// `regionCoveringLine`) — `undefined` only when `line` is somehow out of
  /// range for the current blame result.
  region: BlameRegion | undefined;
  /// The cached-by-sha `/api/why?line=` response for `region.sha`
  /// (`useBlameAttributions`'s `bySha`) — `undefined` while that sha's
  /// lazy prefetch is still in flight.
  why: LineWhyOut | undefined;
}

/// The disclosure ladder's step 3 — W4.4's "three-line why": prompt
/// excerpt + decisions (`why.kb_context`), the region's own commit, a
/// "sibling files" best-effort section (sourced from `GET
/// /api/session-diff`, since neither `/api/why` nor `/api/story` carry a
/// files list of their own — see `lib/sessionDiff.ts`'s doc), a "session
/// also wrote N memories" list (`why.session_memory_ids` — display-only
/// ids, never fetched bodies), a "timeline" expander (`GET
/// /api/blame/timeline`, step 3's set-valued history), and an "open session
/// in kb" link SCOPED with `?kb=` when `attribution.kb` resolved (else the
/// pre-existing unscoped link). Step 4 (honest absence) is this
/// component's OWN copy, not a separate state: `via === "uncommitted-live"`
/// renders "uncommitted — live session evidence", any other
/// `confidence: "none"` renders "no recorded session" — both real
/// `AttributionOut` fields the server already computed, nothing invented
/// client-side.
export default function WhyPanel({ repo, repoRoot, path, line, region, why }: WhyPanelProps) {
  const [timelineOpen, setTimelineOpen] = useState(false);
  const timelineLine = region?.final_start ?? line;
  const timeline = useBlameTimeline(repo, path, timelineLine, timelineOpen);

  const attribution = why?.attribution;
  const sessionId = attribution?.session_id;
  // Loopback-only server-side (`sessiondiff`'s module doc); `isError`
  // degrades this section to absent rather than a hard failure — see
  // `useSessionDiff`'s own doc.
  const sessionDiff = useSessionDiff(sessionId, repo, !!sessionId);
  const siblings =
    sessionId && sessionDiff.data ? siblingFiles(sessionDiff.data, path, repoRoot) : null;

  if (!why || !attribution) {
    return (
      <div className="kbc-why kbc-why--loading" data-kbc-why-line={line}>
        Loading provenance for line {line}…
      </div>
    );
  }

  const isUncommitted = attribution.via === "uncommitted-live";
  const noRecordedSession = attribution.confidence === "none" && !isUncommitted;

  return (
    <div className="kbc-why" data-kbc-why-line={line}>
      <div className="kbc-why__head">
        <span className="kbc-why__line">Line {line}</span>
        <span
          className={`kbc-why__confidence kbc-why__confidence--${attribution.confidence}`}
          data-kbc-why-confidence={attribution.confidence}
        >
          {attribution.confidence}
        </span>
      </div>

      {region && !isUncommitted && (
        <div className="kbc-why__commit">
          {region.sha === UNCOMMITTED_SHA ? (
            <span className="kbc-why__sha">{shortSha(region.sha)}</span>
          ) : (
            <Link to={commitUrl(repo, region.sha)} className="kbc-why__sha">
              {shortSha(region.sha)}
            </Link>
          )}
          <span className="kbc-why__subject">{region.subject}</span>
          <span className="kbc-why__meta">
            {region.author} · {formatUnixSeconds(region.author_time)}
          </span>
        </div>
      )}

      {/* R12 — originating change: parent→sha diff, hunks covering this line. */}
      {region && (
        <OriginatingChange
          repo={repo}
          path={path}
          sha={region.sha}
          previousSha={region.previous_sha}
          line={line}
        />
      )}

      {isUncommitted && (
        <p className="kbc-why__absence" data-kbc-why-absence="uncommitted-live">
          uncommitted — live session evidence
          {attribution.session_ids && attribution.session_ids.length > 0 && (
            <>
              {" "}
              ({attribution.session_ids.length} in-flight session
              {attribution.session_ids.length === 1 ? "" : "s"})
            </>
          )}
        </p>
      )}
      {noRecordedSession && (
        <p className="kbc-why__absence" data-kbc-why-absence="no-session">
          no recorded session
        </p>
      )}

      {why.kb_context && (
        <div className="kbc-why__context">
          {why.kb_context.prompt_excerpt && (
            <blockquote className="kbc-why__prompt">{why.kb_context.prompt_excerpt}</blockquote>
          )}
          {why.kb_context.decisions.length > 0 && (
            <ul className="kbc-why__decisions">
              {why.kb_context.decisions.map((d, i) => (
                <li key={i}>
                  <span className="kbc-why__decision-kind">{d.kind}</span> {d.answer ?? d.prompt ?? ""}
                </li>
              ))}
            </ul>
          )}
        </div>
      )}

      {siblings && siblings.length > 0 && (
        <div className="kbc-why__siblings">
          <div className="kbc-why__siblings-title">Also touched in this session</div>
          <ul>
            {siblings.map((f) => (
              <li key={f}>{f}</li>
            ))}
          </ul>
        </div>
      )}

      {/* Display-only: ids to link out to, never fetched memory bodies —
          see `LineWhyOut.session_memory_ids`'s doc. */}
      {why.session_memory_ids && why.session_memory_ids.length > 0 && (
        <div className="kbc-why__memories" data-kbc-why-memories>
          <div className="kbc-why__memories-title">
            Session also wrote {why.session_memory_ids.length} memor
            {why.session_memory_ids.length === 1 ? "y" : "ies"}
          </div>
          <ul>
            {why.session_memory_ids.slice(0, 5).map((id) => (
              <li key={id}>{id.slice(0, 12)}</li>
            ))}
          </ul>
        </div>
      )}

      {why.timeline_available && (
        <div className="kbc-why__timeline">
          <button
            type="button"
            className="kbc-why__timeline-toggle"
            onClick={() => setTimelineOpen((v) => !v)}
            data-kbc-why-timeline-toggle
          >
            {timelineOpen ? "Hide timeline" : "Show timeline"}
          </button>
          {timelineOpen &&
            (timeline.isLoading ? (
              <div className="kbc-why__timeline-loading">Loading…</div>
            ) : (
              <ul className="kbc-why__timeline-list" data-kbc-why-timeline-list>
                {(timeline.data?.entries ?? []).map((e) => (
                  <li key={e.sha}>
                    {e.sha === UNCOMMITTED_SHA ? (
                      <span className="kbc-why__sha">{shortSha(e.sha)}</span>
                    ) : (
                      <Link to={commitUrl(repo, e.sha)} className="kbc-why__sha">
                        {shortSha(e.sha)}
                      </Link>
                    )}{" "}
                    {e.subject} · {formatUnixSeconds(e.author_time)}
                  </li>
                ))}
              </ul>
            ))}
        </div>
      )}

      {sessionId && (
        <a
          className="kbc-why__open-session"
          href={sessionUrl(sessionId, attribution.kb)}
          target="_blank"
          rel="noreferrer"
          data-kbc-why-open-session
        >
          Open session in kb <Icon.External width={12} height={12} aria-hidden />
        </a>
      )}
    </div>
  );
}
