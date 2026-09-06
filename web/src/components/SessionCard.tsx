import type { SessionRow } from "../api/sessions";
import { humanizeDuration } from "../lib/time";
import { relativeAge } from "../lib/derive";
import { commitBadge, outcomeLine, substanceBadge } from "../lib/sessionChips";
import { sessionDisplayName } from "../lib/sessionDisplayName";

// Session-aware gallery card body. Rendered INSIDE the shared `.kb-card`
// shell (article + action buttons + Link) by Card.tsx when a gallery doc is a
// captured session transcript (kb_category === "memory-session") and a matching
// SessionRow is present. It keeps the card's fixed 246px height (VirtualGrid's
// stride is coupled to it) but swaps the generic title/excerpt/word-tape for
// the readable, session-shaped record:
//
//   working folder (cwd)            ← slug line, not the empty memory-kb folder
//   First user prompt / display     ← heading, not "Session transcript <ts>"
//   » outcome (or first prompt)     ← W3.F/S7: the closure, 2-line clamped
//   N msgs · M edited · <duration>  ← counts strip, not the meaningless word-tape
//   ◆ session ✓N · <age>            ← footer badge + commit glyph + relative age
//
// R4/S7 — a trivial husk stays visible (never hidden) but the whole card
// dims + gets a quiet `∅` badge, same "dimmed, badged, never gone" contract
// as the sessions-list row (S1).
// The join key is SessionRow.artifact_id === DocSummary.id (invariant #27).
export default function SessionCardBody({ session }: { session: SessionRow }) {
  const folder = session.folder || "";
  const counts: string[] = [`${session.message_count} msg`];
  if (session.files_edited_count > 0)
    counts.push(`${session.files_edited_count} edited`);
  if (session.files_read_count > 0)
    counts.push(`${session.files_read_count} read`);
  const dur = humanizeDuration(session.duration_ms);
  if (dur) counts.push(dur);
  const age = relativeAge(session.started_at);
  const preview = outcomeLine(session.outcome, session.first_user_prompt);
  const husk = substanceBadge(session.substance);

  return (
    <>
      <div
        className="kb-card__folder kb-card__sfolder"
        title={session.cwd || folder || "no working directory"}
      >
        {folder || "—"}
      </div>
      <h3 className="kb-card__title kb-card__stitle">
        {sessionDisplayName(session)}
      </h3>
      {preview && (
        <p className="kb-card__soutcome">
          {preview.isOutcome && <span className="kb-card__soutcome-mark">» </span>}
          {preview.text}
        </p>
      )}
      <div className="kb-card__smeta">
        {counts.map((c, i) => (
          <span key={i}>
            {i > 0 && <span className="kb-card__sdot"> · </span>}
            {c}
          </span>
        ))}
      </div>
      <div className="kb-card__foot kb-card__sfoot">
        <span className="kb-card__sbadge">◆ session</span>
        {commitBadge(session.commit_count) && (
          <span className="kb-card__scommits" title={`${session.commit_count} commits`}>
            {commitBadge(session.commit_count)}
          </span>
        )}
        {husk && (
          <span className="kb-card__shusk" title="trivial capture — no real content">
            {husk}
          </span>
        )}
        {session.model && (
          <span className="kb-card__smodel" title={session.model}>
            {session.model.replace(/^claude-/, "")}
          </span>
        )}
        {age && <span className="kb-card__age">· {age}</span>}
      </div>
    </>
  );
}
