import { Icon } from "../icons";
import { glyphForKind, authorChip, opacityForAge } from "../../lib/slateGlyphs";
import { ageLabel } from "./SlateCard";
import { GENERAL_LANE, seenChipFor, type NowRow } from "../../lib/slateLanes";
import type { SlateBoardCard } from "../../api/slateTypes";

// The full-width NOW band: one row per topic with its now line, then the
// WARN rows (§10 "Desktop"). `role="status"` — this is the one region whose
// content is "what would change your next action", so a change here should
// reach a screen-reader user without them going looking. It is sticky on
// mobile for the same reason.
//
// A WARN never ages out (rules matrix "Warn lifetime": age is DISPLAYED and
// the board may fade it, but it never expires), so warn rows carry the same
// age fade as everything else and are never dropped from this band.

type Props = {
  rows: NowRow[];
  warns: SlateBoardCard[];
  /// The "post" button that opens the composer (§10 "Composer").
  onCompose?: () => void;
  onOpenHistory?: () => void;
  /// Topic filter chips — `?topic=` (the `t` key opens this).
  topics?: readonly string[];
  activeTopic?: string | null;
  onPickTopic?: (topic: string | null) => void;
  swimlanes?: boolean;
  onToggleSwimlanes?: () => void;
  closed?: boolean;
};

/// D27 (v0.42) — `seen by N`, hover-listing the sessions the daemon has
/// SERVED this post to (never "read": a cursor is attribution, D27). The
/// gate, the label and the hover all come from `slateLanes.seenChipFor`,
/// which `SlateCard` also calls.
function SeenChip({ card }: { card: SlateBoardCard }) {
  const seen = seenChipFor(card);
  if (!seen) return null;
  return (
    <span className="slate-card__seen slate-now__seen" title={seen.title}>
      {seen.label}
    </span>
  );
}

export default function NowBand({
  rows,
  warns,
  onCompose,
  onOpenHistory,
  topics = [],
  activeTopic = null,
  onPickTopic,
  swimlanes = false,
  onToggleSwimlanes,
  closed = false,
}: Props) {
  const now = glyphForKind("now");
  const warn = glyphForKind("warn");

  return (
    <section className="slate-now" role="status" aria-label="Now and warnings">
      <div className="slate-now__bar">
        <h2 className="slate-now__title">
          <span aria-hidden="true">{now.glyph}</span> Now
        </h2>
        {closed && <span className="slate-now__closed">closed</span>}
        <div className="slate-now__spacer" />
        {topics.length > 0 && onPickTopic && (
          <div className="slate-now__topics" role="group" aria-label="Filter by topic">
            <button
              type="button"
              className={`slate-topic${activeTopic === null ? " is-on" : ""}`}
              aria-pressed={activeTopic === null}
              onClick={() => onPickTopic(null)}
            >
              all
            </button>
            {topics.map((t) => (
              <button
                key={t}
                type="button"
                className={`slate-topic${activeTopic === t ? " is-on" : ""}`}
                aria-pressed={activeTopic === t}
                onClick={() => onPickTopic(t)}
              >
                {t}
              </button>
            ))}
          </div>
        )}
        {onToggleSwimlanes && (
          <button
            type="button"
            className="slate-now__act"
            aria-pressed={swimlanes}
            data-act="swimlanes"
            onClick={onToggleSwimlanes}
          >
            <Icon.Table /> swimlanes
          </button>
        )}
        {onOpenHistory && (
          <button
            type="button"
            className="slate-now__act"
            data-act="history"
            onClick={onOpenHistory}
          >
            <Icon.History /> history
          </button>
        )}
        {onCompose && (
          <button
            type="button"
            className="slate-now__act slate-now__act--primary"
            data-act="compose"
            onClick={onCompose}
          >
            <Icon.Plus /> post
          </button>
        )}
      </div>

      {rows.length === 0 && warns.length === 0 ? (
        <p className="slate-now__empty">
          No NOW line yet — post one to say what is in flight.
        </p>
      ) : (
        <ul className="slate-now__rows">
          {rows.map((r) => (
            <li
              key={r.card.seq}
              className="slate-now__row"
              style={{ opacity: opacityForAge(r.card.age_secs) }}
            >
              <span className="slate-now__kind">
                <span aria-hidden="true">{now.glyph}</span> NOW
              </span>
              <span className="slate-now__lane">{r.label || GENERAL_LANE}</span>
              <span className="slate-now__seq">#{r.card.seq}</span>
              <span className="slate-now__line">{r.card.line}</span>
              <span className="slate-now__who">
                {authorChip(r.card.who)} · {ageLabel(r.card.age_secs)}
              </span>
              {/* D27 — the same chip the columns render, from the same
                  helper, so the band and a HAND card can never disagree
                  about who has been served what. A WARN row carries none:
                  the digest gates on NOW/HAND/ASK, and the board follows it
                  rather than inventing a fourth line that says more. */}
              <SeenChip card={r.card} />
            </li>
          ))}
          {warns.map((w) => (
            <li
              key={w.seq}
              className="slate-now__row slate-now__row--warn"
              style={{ opacity: opacityForAge(w.age_secs) }}
            >
              <span className="slate-now__kind">
                <span aria-hidden="true">{warn.glyph}</span> WARN
              </span>
              <span className="slate-now__lane">{w.topic ?? GENERAL_LANE}</span>
              <span className="slate-now__seq">#{w.seq}</span>
              <span className="slate-now__line">{w.line}</span>
              <span className="slate-now__who">
                {authorChip(w.who)} · {ageLabel(w.age_secs)}
              </span>
            </li>
          ))}
        </ul>
      )}
    </section>
  );
}
