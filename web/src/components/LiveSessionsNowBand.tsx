// LSC-4 — the "Now" band: three live lanes (in progress / waiting on you /
// finished) rendered above the /sessions day-buckets (design §7 "the human
// interface"). Deliberately NOT a new route/dashboard — §10's recorded
// refusal ("don't rebuild a terminal dashboard") is why this lives inside
// the existing /sessions page instead.
//
// Purely presentational: `sessions.tsx` calls `useLiveSessions()` and the
// shared `now` tick (`useNowTick`, already threaded through the page) and
// passes them in — same split CodeRefsSection.tsx follows (fetch/hook
// plumbing stays with the caller; this file just renders). Kept in its own
// file (rather than inline in the ~2000-line sessions.tsx, which already
// hosts many local sub-components) specifically so it gets direct,
// lightweight component coverage without mounting the whole route.
//
// The row action is "copy the resume command" — ONE primary action (design
// §7). It reuses the row's OWN precomputed `resume` string (LSC-2's wire
// contract: `claude -r <sid>` for Claude, the harness-correct equivalent for
// codex/grok/kimi/opencode) rather than sessions.tsx's `ResumeCommand`
// component, which hardcodes `claude -r ${sessionId}` — reusing it
// unmodified here would silently emit a WRONG resume command for every
// non-Claude row. This is the harness-correctness reason the wire schema
// carries a precomputed string in the first place (design §5); rebuilding
// it client-side would be the actual reimplementation to avoid.

import { useState } from "react";
import { Icon } from "./icons";
import { relativeAge } from "../lib/time";
import { harnessGlyph } from "../lib/sessionChips";
import type { LiveStatusRow } from "../api/sessions";
import {
  bucketLiveSessions,
  isLongWait,
  isPresumedEnded,
  isStalled,
  liveHonestyKind,
  liveHonestyLabel,
  liveHonestyTitle,
  FINISHED_LANE_CAP,
} from "../lib/liveSessionLanes";

export default function LiveSessionsNowBand({
  rows,
  now,
}: {
  rows: LiveStatusRow[];
  now: number;
}) {
  const lanes = bucketLiveSessions(rows);
  const empty =
    lanes.inProgress.length === 0 &&
    lanes.waiting.length === 0 &&
    lanes.finished.length === 0;
  // A quiet fleet costs zero visual noise — the whole band collapses.
  if (empty) return null;
  return (
    <div className="kb-now" data-testid="sessions-now-band" aria-label="live sessions">
      {lanes.inProgress.length > 0 && (
        <section className="kb-now__lane kb-now__lane--progress" data-testid="now-lane-progress">
          <h3 className="kb-now__lane-head">
            In progress
            <span className="kb-now__lane-count">{lanes.inProgress.length}</span>
          </h3>
          <ol className="kb-now__rows">
            {lanes.inProgress.map((r) => (
              <LiveRow key={r.session_id} row={r} now={now} kind="progress" />
            ))}
          </ol>
        </section>
      )}
      {lanes.waiting.length > 0 && (
        <section
          id="kb-now-waiting"
          className="kb-now__lane kb-now__lane--waiting"
          data-testid="now-lane-waiting"
        >
          <h3 className="kb-now__lane-head">
            Waiting on you
            <span className="kb-now__lane-count">{lanes.waiting.length}</span>
          </h3>
          <ol className="kb-now__rows">
            {lanes.waiting.map((r) => (
              <LiveRow key={r.session_id} row={r} now={now} kind="waiting" />
            ))}
          </ol>
        </section>
      )}
      {lanes.finished.length > 0 && (
        <section className="kb-now__lane kb-now__lane--finished" data-testid="now-lane-finished">
          <h3 className="kb-now__lane-head">
            Finished
            <span className="kb-now__lane-count">{lanes.finished.length}</span>
          </h3>
          {lanes.finishedTotal > FINISHED_LANE_CAP && (
            <div className="kb-now__lane-caption">
              showing {lanes.finished.length} most recent of {lanes.finishedTotal} — the day-buckets
              below list the rest
            </div>
          )}
          <ol className="kb-now__rows">
            {lanes.finished.map((r) => (
              <LiveRow key={r.session_id} row={r} now={now} kind="finished" />
            ))}
          </ol>
        </section>
      )}
    </div>
  );
}

function LiveRow({
  row,
  now,
  kind,
}: {
  row: LiveStatusRow;
  now: number;
  kind: "progress" | "waiting" | "finished";
}) {
  const honesty = liveHonestyKind(row);
  // Primary identifier: project (the thing the operator actually scans
  // for) when known, else title/cwd/id as honest fallbacks. Secondary line
  // prefers the title/last-line — whichever wasn't already used as the
  // primary — so the two lines never repeat the same fact.
  const primary = row.project || row.title || row.cwd || row.session_id;
  const secondary = row.project && row.title ? row.title : row.last_line;
  const elapsed = relativeAge(row.since_unix, now);
  return (
    <li className="kb-now__row" data-testid="now-row" data-live-state={row.state}>
      <span
        className="kb-now__row-harness"
        title={row.harness}
        aria-hidden="true"
      >
        {harnessGlyph(row.harness)}
      </span>
      <span className="kb-now__row-body">
        <span className="kb-now__row-line1">
          <span className="kb-now__row-time" title="time in this state">
            {elapsed}
          </span>
          <span className="kb-now__row-project">{primary}</span>
          {kind === "progress" && isStalled(row) && (
            <span
              className="kb-now__row-doubt"
              data-testid="now-row-stalled"
              title="the agent still holds the turn, but this is well past any plausible single tool call — silence is not evidence it's actually waiting on you"
            >
              stalled?
            </span>
          )}
          {kind === "waiting" && isLongWait(row) && (
            <span
              className="kb-now__row-doubt"
              data-testid="now-row-longwait"
              title="waiting more than a working day — still on your plate, just older"
            >
              long wait
            </span>
          )}
          {kind === "finished" && isPresumedEnded(row) && (
            <span
              className="kb-now__row-doubt"
              data-testid="now-row-presumed-ended"
              title="no SessionEnd signal fired — the lease simply expired with nothing landing since. An inference, not an observed end."
            >
              presumed ended
            </span>
          )}
          {honesty && (
            <span
              className="kb-now__row-honesty"
              data-testid="now-row-honesty"
              data-kb-live-honesty={honesty}
              title={liveHonestyTitle(honesty)}
            >
              {liveHonestyLabel(honesty)}
            </span>
          )}
        </span>
        {secondary && (
          <span className="kb-now__row-line2" title={row.last_line ?? undefined}>
            {secondary}
          </span>
        )}
      </span>
      <CopyResumeButton resume={row.resume} />
    </li>
  );
}

/// The row's ONE primary action (design §7): copy the exact, harness-correct
/// resume command the server already computed. Same inline copy-with-
/// timeout pattern `ResumeCommand`/`ResumeSection` use in sessions.tsx
/// (duplicated there twice already — a third small copy here follows that
/// established shape rather than inventing a shared abstraction for a few
/// lines).
function CopyResumeButton({ resume }: { resume: string }) {
  const [copied, setCopied] = useState(false);
  const copy = () => {
    void navigator.clipboard?.writeText(resume).then(
      () => {
        setCopied(true);
        setTimeout(() => setCopied(false), 1500);
      },
      () => {},
    );
  };
  return (
    <button
      type="button"
      className="kb-now__row-resume"
      onClick={copy}
      data-testid="now-row-resume-copy"
      title={resume}
    >
      {copied ? <Icon.Check aria-hidden /> : <Icon.Copy aria-hidden />}
      {copied ? "copied" : "resume"}
    </button>
  );
}
