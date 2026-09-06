// W3.E/S3 — the reader's session identity strip: rendered between ContextBar
// and the iframe when the open artifact IS a session capture (architecture
// (c) — the reader stays THE artifact home; this is one slim strip, not a
// second route/rail-icon, per S3's defense-of-(c) over a native `/session/:sid`
// route or a rail-only adaptation). Data comes from the by-artifact join
// (#11's canonical sqlite lookup) — never the filename regex the deleted
// `SessionSelfLink` used.
//
// "ONE action home" (synthesis-memo R11/S-S3): worklog + replay live HERE,
// not duplicated on the rail's Sessions tab (`PreviewInspector`'s
// `SessionProducedSection` dropped its own copies of both links).

import { useState } from "react";
import { Link } from "react-router-dom";
import type { SessionRow } from "../../api/sessions";
import {
  commitBadge,
  errorBadge,
  harnessGlyph,
  memoryBadge,
  outcomeLine,
} from "../../lib/sessionChips";
import { sessionPresence, type PresenceLiveSet } from "../../lib/sessionPresence";
import { replayUrl, sessionsWorklogUrl } from "../../lib/sessionsUrl";
import { sessionDisplayName } from "../../lib/sessionDisplayName";

const EXPANDED_KEY_PREFIX = "kb:sesctx:";

function readExpanded(artifactId: string): boolean {
  try {
    return sessionStorage.getItem(EXPANDED_KEY_PREFIX + artifactId) === "1";
  } catch {
    return false;
  }
}

function writeExpanded(artifactId: string, on: boolean): void {
  try {
    sessionStorage.setItem(EXPANDED_KEY_PREFIX + artifactId, on ? "1" : "0");
  } catch {
    // sessionStorage unavailable (private mode) — the toggle still works,
    // it just resets to collapsed next visit.
  }
}

export default function SessionContextCard({
  session,
  newest,
  onJumpToOutcome,
  presenceSet,
  followMode,
  onToggleFollow,
}: {
  session: SessionRow;
  newest: boolean;
  onJumpToOutcome: () => void;
  /// W7 (R15/LF-1) — Tier-1 presence set (empty when Tier-1 is unconfigured
  /// or the daemon is non-loopback — the card degrades to the Tier-0 chip
  /// automatically). Optional so every pre-W7 call site keeps compiling;
  /// defaults to "no Tier-1 evidence".
  presenceSet?: PresenceLiveSet;
  /// W7 (R15/LF-2) — follow-mode state, lifted to the route (`?follow=1`)
  /// so it survives the handoff reload. `undefined` hides the chip
  /// entirely (a non-session caller, or before the route has resolved).
  followMode?: boolean;
  onToggleFollow?: () => void;
}) {
  const [expanded, setExpanded] = useState(() => readExpanded(session.artifact_id));
  const toggle = () => {
    const next = !expanded;
    setExpanded(next);
    writeExpanded(session.artifact_id, next);
  };
  const preview = outcomeLine(session.outcome, session.first_user_prompt);
  // W5/R9b/LF-1 — staleness badge, beside the presence chip (memo LF-1:
  // "SessionContextCard shows the chip beside the staleness badge"); W7
  // upgrades to Tier 1 when `presenceSet` has evidence.
  const presence = sessionPresence(session, Date.now(), presenceSet);
  // LF-2 — "a Follow chip in the live header (present when LF-1 reports
  // live/active)".
  const showFollowChip =
    onToggleFollow !== undefined && presence.status !== "idle";

  return (
    <div className="kb-sesctx" data-testid="session-context-card">
      <div className="kb-sesctx__row">
        <button
          type="button"
          className="kb-sesctx__toggle"
          onClick={toggle}
          aria-expanded={expanded}
          title={expanded ? "collapse" : "expand"}
          data-testid="session-context-toggle"
        >
          {expanded ? "▾" : "▸"}
        </button>
        <span className="kb-sesctx__harness" title={session.harness} aria-hidden>
          {harnessGlyph(session.harness)}
        </span>
        <span className="kb-sesctx__name">{sessionDisplayName(session)}</span>
        {preview && (
          <span className="kb-sesctx__outcome">
            {preview.isOutcome && <span className="kb-sesctx__mark">» </span>}
            {preview.text}
          </span>
        )}
        <span className="kb-sesctx__chips">
          {commitBadge(session.commit_count) && (
            <span>{commitBadge(session.commit_count)}</span>
          )}
          {errorBadge(session.error_count) && (
            <span>{errorBadge(session.error_count)}</span>
          )}
          {memoryBadge(session.memory_count) && (
            <span>{memoryBadge(session.memory_count)}</span>
          )}
          <span
            className={`kb-sesctx__presence kb-sesctx__presence--${presence.status}`}
            title={presence.copy}
            data-testid="session-context-presence"
          >
            {presence.copy}
          </span>
          {!newest && (
            <span
              className="kb-sesctx__superseded"
              title="a newer capture of this session exists — this is a superseded snapshot"
            >
              superseded
            </span>
          )}
        </span>
        <span className="kb-sesctx__actions">
          {showFollowChip && (
            <button
              type="button"
              className={`kb-sesctx__act kb-sesctx__follow${followMode ? " is-active" : ""}`}
              onClick={onToggleFollow}
              data-testid="session-follow-toggle"
              aria-pressed={!!followMode}
              title={
                followMode
                  ? "stop following — return to the normal reader"
                  : "follow this session: auto-advance to new activity"
              }
            >
              {followMode ? "● following" : "follow"}
            </button>
          )}
          <button
            type="button"
            className="kb-sesctx__act"
            onClick={onJumpToOutcome}
            data-testid="session-jump-outcome"
            title="jump to the outcome"
          >
            ↧ outcome
          </button>
          <Link
            className="kb-sesctx__act"
            to={replayUrl(session.kb, session.session_id)}
            data-testid="session-context-replay"
          >
            replay →
          </Link>
          <Link
            className="kb-sesctx__act"
            to={sessionsWorklogUrl(session.session_id, session.project_key)}
            data-testid="session-context-worklog"
          >
            worklog →
          </Link>
        </span>
      </div>
      {expanded && (
        <div className="kb-sesctx__full">
          {session.outcome ? (
            <p className="kb-sesctx__full-text">{session.outcome}</p>
          ) : (
            <p className="kb-sesctx__full-text kb-sesctx__full-text--empty">
              no closing prose recorded for this session.
            </p>
          )}
        </div>
      )}
    </div>
  );
}
