import { useEffect, useMemo, useState } from "react";
import { Link } from "react-router-dom";
import type { DocSummary } from "../api/client";
import {
  assembleBiography,
  splitBiography,
  type BioEvent,
} from "../lib/biography";
import { sessionColorFor } from "../lib/sessionColor";
import { relativeAge } from "../lib/time";
import { toast } from "../lib/toast";
import { useArtifactSessions } from "../hooks/useSessions";
import { useReview } from "../hooks/useReview";
import { useVersions } from "../hooks/useVersions";
import PromptPanel from "./PromptPanel";

// W2.1 — the reader inspector's "Story" tab: a single quiet vertical
// timeline merging the artifact's origin session, every session that later
// touched it (with its steering decisions + git commits), a per-artifact
// comments beat, and its version history. Every row deep-links to an
// EXISTING surface — invariant #30 (a new sub-tab, never a second rail): no
// new endpoints, no new panel. Mirrors SessionsTouchedPanel/
// RelatedMemoriesPanel — self-fetching, mounted only inside `show("story")`
// (including the merged "all" tab). `onCountChange` reports the assembled
// length up to PreviewInspector's rail badge so the count stays correct
// without a second, duplicate fetch at the top level.
//
// The generating prompt (`<template id="kb-prompt">`) — DEFERRED through
// W2.10, now real (W2.11): `PromptPanel` renders at the head of this tab,
// self-fetching `GET /api/kb/{kb}/artifacts/{id}/prompt`. It stays outside
// `events`/`onCountChange` on purpose — it isn't a dated timeline beat like
// a session/decision/commit, it's the artifact's own birth bundle, always
// pinned above the (chronological) story rather than sorted into it.
export default function BiographyTab({
  kb,
  doc,
  onComments,
  onVersions,
  onCountChange,
}: {
  kb: string;
  doc: DocSummary;
  onComments?: () => void;
  onVersions?: () => void;
  onCountChange?: (n: number) => void;
}) {
  const { sessions, loading: sessionsLoading } = useArtifactSessions(
    kb,
    doc.id,
  );
  // Shares the SAME ["review", kb, artifactId] cache entry Detail's own
  // useReview call already warmed — no extra network round-trip.
  const review = useReview(kb, doc.id);
  const { versions, loading: versionsLoading } = useVersions(kb, doc.id, true);

  const events = useMemo(
    () =>
      assembleBiography({
        sessions,
        comments: review.file?.comments ?? [],
        versions,
      }),
    [sessions, review.file, versions],
  );

  useEffect(() => {
    onCountChange?.(events.length);
  }, [events.length, onCountChange]);

  const [expanded, setExpanded] = useState(false);
  const { visible, collapsed } = splitBiography(events);
  const shown = expanded ? events : visible;
  const loading = sessionsLoading || review.loading || versionsLoading;

  if (loading && events.length === 0) {
    return (
      <>
        <h4>Story</h4>
        <PromptPanel kb={kb} artifactId={doc.id} />
        <div className="kb-pinsp__hint">loading…</div>
      </>
    );
  }
  if (events.length === 0) {
    return (
      <>
        <h4>Story</h4>
        <PromptPanel kb={kb} artifactId={doc.id} />
        <div className="kb-pinsp__hint">
          No sessions, comments, or versions recorded for this artifact yet.
        </div>
      </>
    );
  }

  return (
    <>
      <h4>Story</h4>
      <PromptPanel kb={kb} artifactId={doc.id} />
      <ol className="kb-bio">
        {shown.map((e) => (
          <BioRow
            key={e.id}
            event={e}
            onComments={onComments}
            onVersions={onVersions}
          />
        ))}
      </ol>
      {!expanded && collapsed.length > 0 && (
        <button
          type="button"
          className="kb-bio__more"
          data-kb-act="story-expand"
          onClick={() => setExpanded(true)}
        >
          {collapsed.length} earlier event{collapsed.length === 1 ? "" : "s"}
        </button>
      )}
    </>
  );
}

function sessionFocusUrl(sessionKb: string, sessionId: string): string {
  return `/sessions?kb=${encodeURIComponent(sessionKb)}&focus=${encodeURIComponent(sessionId)}`;
}

// dot + rail line; content varies by kind below. Only session-rooted kinds
// (origin/session/decision/commit) get a colored dot (the shared session's
// color) — comments/version beats stay a neutral muted dot, per the calm-
// computing brief ("no colors beyond the session-color dot for session
// rows").
function BioRow({
  event,
  onComments,
  onVersions,
}: {
  event: BioEvent;
  onComments?: () => void;
  onVersions?: () => void;
}) {
  const dotColor =
    event.kind === "origin" ||
    event.kind === "session" ||
    event.kind === "decision" ||
    event.kind === "commit"
      ? sessionColorFor(event.sessionId)
      : undefined;
  return (
    <li className={`kb-bio__row kb-bio__row--${event.kind}`}>
      <span className="kb-bio__spine" aria-hidden>
        <span
          className="kb-bio__dot"
          style={dotColor ? { background: dotColor } : undefined}
        />
      </span>
      <div className="kb-bio__body">
        <BioRowContent
          event={event}
          onComments={onComments}
          onVersions={onVersions}
        />
      </div>
    </li>
  );
}

function BioRowContent({
  event,
  onComments,
  onVersions,
}: {
  event: BioEvent;
  onComments?: () => void;
  onVersions?: () => void;
}) {
  switch (event.kind) {
    case "origin":
    case "session": {
      const url = sessionFocusUrl(event.kb, event.sessionId);
      const label = event.kind === "origin" ? "Origin" : "Session";
      return (
        <>
          <Link
            to={url}
            className="kb-bio__head"
            title={`${event.kb} · ${relativeAge(event.unix)}`}
          >
            <span
              className={`kb-bio__kindlbl kb-bio__kindlbl--${event.kind}`}
            >
              {label}
            </span>
            <span className="kb-bio__title">{event.displayName}</span>
          </Link>
          <div className="kb-bio__row2">
            <BioActionChips
              read={event.read}
              wrote={event.wrote}
              edited={event.edited}
            />
            {/* W3.R-c — jump straight from the artifact's biography into the
                replay of the session that touched it. */}
            <Link
              className="kb-bio__replay"
              to={`/replay/${encodeURIComponent(event.kb)}/${encodeURIComponent(event.sessionId)}`}
              title="Replay this session beat by beat"
            >
              replay
            </Link>
            <span className="kb-bio__ago">{relativeAge(event.unix)}</span>
          </div>
          {event.firstUserPrompt && (
            <p className="kb-bio__excerpt">{event.firstUserPrompt}</p>
          )}
        </>
      );
    }
    case "decision": {
      const url = sessionFocusUrl(event.kb, event.sessionId);
      return (
        <>
          <Link
            to={url}
            className="kb-bio__head"
            title={`${event.kb} · ${relativeAge(event.unix)}`}
          >
            <span className="kb-bio__kindlbl kb-bio__kindlbl--decision">
              {event.decisionKind === "plan" ? "Plan" : "Decision"}
            </span>
            <span className="kb-bio__ago">{relativeAge(event.unix)}</span>
          </Link>
          <p className="kb-bio__excerpt">{event.prompt}</p>
          {event.answer && <p className="kb-bio__answer">→ {event.answer}</p>}
        </>
      );
    }
    case "commit": {
      const url = sessionFocusUrl(event.kb, event.sessionId);
      return (
        <>
          <div className="kb-bio__head">
            <Link
              to={url}
              className="kb-bio__kindlbl kb-bio__kindlbl--commit"
              title={`${event.kb} · ${relativeAge(event.unix)}`}
            >
              Commit
            </Link>
            <CopyShaChip sha={event.sha} />
            {/* "Detected, not ground truth" — borrowed from kb-code's
                Attribution ladder (look only); an unresolved sha never
                gets a fabricated label beyond its own prefix. */}
            <span
              className={`kb-bio__chip kb-bio__chip--${event.resolved ? "resolved" : "unresolved"}`}
              title={
                event.resolved
                  ? "resolved against the local git history at capture time"
                  : "not resolved — sha prefix only"
              }
            >
              {event.resolved ? "resolved" : "unresolved"}
            </span>
            <span className="kb-bio__ago">{relativeAge(event.unix)}</span>
          </div>
          {event.subject && <p className="kb-bio__excerpt">{event.subject}</p>}
        </>
      );
    }
    case "comments": {
      const parts: string[] = [];
      if (event.openCount > 0) parts.push(`${event.openCount} open`);
      if (event.resolvedCount > 0)
        parts.push(`${event.resolvedCount} resolved`);
      return (
        <button
          type="button"
          className="kb-bio__head kb-bio__head--btn"
          data-kb-act="story-comments"
          onClick={onComments}
          disabled={!onComments}
        >
          <span className="kb-bio__kindlbl kb-bio__kindlbl--comments">
            Comments
          </span>
          <span className="kb-bio__title">{parts.join(" · ")}</span>
          <span className="kb-bio__ago">{relativeAge(event.unix)}</span>
        </button>
      );
    }
    case "version": {
      return (
        <button
          type="button"
          className="kb-bio__head kb-bio__head--btn"
          data-kb-act="story-versions"
          onClick={onVersions}
          disabled={!onVersions}
        >
          <span className="kb-bio__kindlbl kb-bio__kindlbl--version">
            Version
          </span>
          <span className="kb-bio__title">{event.label || event.short}</span>
          <span className="kb-bio__ago">{relativeAge(event.unix)}</span>
        </button>
      );
    }
  }
}

// The W/E/R action badges — same visual language + CSS classes as
// PreviewInspector's SessionsTouchedPanel rows (`.kb-pinsp__act-chip*`),
// reused here rather than duplicated.
function BioActionChips({
  read,
  wrote,
  edited,
}: {
  read: boolean;
  wrote: boolean;
  edited: boolean;
}) {
  if (!read && !wrote && !edited) return null;
  return (
    <span className="kb-pinsp__act-chips" aria-hidden>
      {wrote && (
        <span className="kb-pinsp__act-chip kb-pinsp__act-chip--wrote">W</span>
      )}
      {edited && (
        <span className="kb-pinsp__act-chip kb-pinsp__act-chip--edited">E</span>
      )}
      {read && (
        <span className="kb-pinsp__act-chip kb-pinsp__act-chip--read">R</span>
      )}
    </span>
  );
}

// v0.22's IdCopy pattern (PreviewInspector.tsx), mirrored here for the
// commit sha — copies the FULL prefix shown to the clipboard with toast
// feedback (invariant #32).
function CopyShaChip({ sha }: { sha: string }) {
  return (
    <button
      type="button"
      className="kb-bio__sha"
      data-kb-act="copy-sha"
      title="copy sha"
      aria-label="copy sha"
      onClick={() => {
        navigator.clipboard
          ?.writeText(sha)
          .then(() => toast.ok("sha copied"))
          .catch(() => toast.err("couldn't copy sha"));
      }}
    >
      {sha}
    </button>
  );
}
