import { useMemo, useState } from "react";
import { Link } from "react-router-dom";
import CommentBody from "../CommentBody";
import { Icon } from "../icons";
import {
  authorChip,
  colorForKind,
  glyphForKind,
  glyphForLiveness,
  opacityForAge,
  STATE_GLYPHS,
} from "../../lib/slateGlyphs";
import { codeSearchUrl } from "../../lib/codeLensUrl";
import { seenChipFor } from "../../lib/slateLanes";
import {
  captionsOn,
  groundingTitle,
  type Groundedness,
} from "../../lib/slateGrounding";
import { useSlateGrounding } from "../../hooks/useSlateGrounding";
import type { SlateBoardCard, SlateRefDisplay } from "../../api/slateTypes";

// One card = one post, as the board draws it. EVERY visual device here is a
// rendering of a field the projection already carries (§4 affordance map):
// size from `tier`, opacity from `age_secs`, the ring from `marks`, the
// badge from `liveness`/`contested`, `(was #n)` from `was`. Nothing is
// scored, nothing is author-chosen, and nothing here is a fact the CLI
// cannot print — the CLI just prints it as words.
//
// ACCESSIBILITY (§10): the card is an <article>; the kind WORD comes first
// in reading order and the emoji beside it is aria-hidden, so nothing is
// conveyed by glyph or colour alone.

export type SlateCardActions = {
  onMark?: (card: SlateBoardCard) => void;
  onDrop?: (card: SlateBoardCard) => void;
  onEdit?: (card: SlateBoardCard) => void;
  onPin?: (card: SlateBoardCard, pin: boolean) => void;
  onDone?: (card: SlateBoardCard) => void;
  onAnswer?: (card: SlateBoardCard) => void;
  onTake?: (card: SlateBoardCard) => void;
  /// `post:#n` chip / `(was #n)` link — scroll to and flash a seq.
  onJumpToSeq?: (seq: number) => void;
  /// `(was #n)` opens the history drawer at the ancestor.
  onOpenHistoryAt?: (seq: number) => void;
};

type Props = {
  card: SlateBoardCard;
  /// The kb whose `code_url` resolves `path:` refs, when one is configured.
  codeUrl?: string | null;
  /// The kb the `code_url` above belongs to — D29's caption query is
  /// kb-scoped (kb-code gates every doclens read on its `[doclens] kbs`
  /// allowlist). Absent ⇒ no caption is ever fetched.
  kb?: string | null;
  /// SL7f (v0.42 amendment) — the board route's own slug, sent as the
  /// caption query's `?repo=` (a slate slug IS the kb-code repo name by
  /// design). Absent ⇒ the caption query omits `?repo=` and a multi-repo
  /// kb-code 400s `repo_required`, which the hook already renders `unknown`.
  repo?: string | null;
  /// Desktop shows every action; mobile shows mark/drop/done only (rules
  /// matrix "Mobile" — a reader with a capture slot).
  mobile?: boolean;
  /// Pin is a UI convenience shown when the identity is the operator; the
  /// daemon checks only `origin: human` (§10 "Actions").
  isOperator?: boolean;
  /// The j/k cursor sits on this card.
  focused?: boolean;
  /// Flash it (a `post:#n` jump, or a fresh arrival).
  flashing?: boolean;
  actions?: SlateCardActions;
};

/// Compact relative age. `time.ts`'s `relativeAge` speaks unix-seconds
/// TIMESTAMPS; the projection gives us a DURATION already computed against
/// the daemon's clock (D5 — the daemon has no clock acting on content, so
/// `age_secs` is the honest value, not a second client-side subtraction).
export function ageLabel(secs: number): string {
  if (!Number.isFinite(secs) || secs < 0) return "";
  if (secs < 60) return `${Math.floor(secs)}s`;
  if (secs < 3600) return `${Math.floor(secs / 60)}m`;
  if (secs < 86400) return `${Math.floor(secs / 3600)}h`;
  return `${Math.floor(secs / 86400)}d`;
}

/// One ref chip. `path:` links into kb-code when the kb has a `code_url`
/// and renders as plain text when it does not — never a dead link. `kb:` and
/// `session:` are in-SPA routes; `post:#n` scrolls the board.
function RefChip({
  r,
  codeUrl,
  caption,
  onJumpToSeq,
}: {
  r: SlateRefDisplay;
  codeUrl?: string | null;
  /// D29 — kb-code's answer for THIS ref, when one has settled. Undefined
  /// means no question was asked (no `code_url`) or it is still in flight;
  /// either way nothing renders. There is no "assumed grounded".
  caption?: Groundedness;
  onJumpToSeq?: (seq: number) => void;
}) {
  const raw = r.raw;
  const label = r.display || raw;
  const cls = `slate-ref${r.resolved ? "" : " slate-ref--unresolved"}`;
  // The caption rides OUTSIDE the link/`<span>` so it is never part of the
  // chip's own label or href — it is kb-code talking about the ref, not the
  // ref itself (invariant #2: the class is minted elsewhere, per request,
  // and stored nowhere).
  const cap = caption ? (
    <span
      className="slate-ref__cap"
      data-grounded={caption}
      title={groundingTitle(caption)}
    >
      {caption}
    </span>
  ) : null;

  if (raw.startsWith("path:")) {
    const path = raw.slice(5);
    if (codeUrl) {
      return (
        <span className="slate-ref__wrap">
          <a
            className={cls}
            href={codeSearchUrl(codeUrl, undefined, `#${path.split(":")[0]}`)}
            target="_blank"
            rel="noreferrer"
          >
            {label}
          </a>
          {cap}
        </span>
      );
    }
    return <span className={cls}>{label}</span>;
  }
  if (raw.startsWith("kb:")) {
    const rest = raw.slice(3);
    const slash = rest.indexOf("/");
    if (slash > 0) {
      const kb = rest.slice(0, slash);
      const id = rest.slice(slash + 1);
      return (
        <Link
          className={cls}
          to={`/a/${encodeURIComponent(kb)}/${id}`}
        >
          {label}
        </Link>
      );
    }
    return <span className={cls}>{label}</span>;
  }
  if (raw.startsWith("session:")) {
    return (
      <Link className={cls} to={`/sessions?focus=${encodeURIComponent(raw.slice(8))}`}>
        {label}
      </Link>
    );
  }
  if (raw.startsWith("post:")) {
    const seq = Number(raw.slice(5).replace(/^#/, ""));
    if (Number.isFinite(seq) && onJumpToSeq) {
      return (
        <button
          type="button"
          className={`${cls} slate-ref--btn`}
          onClick={() => onJumpToSeq(seq)}
        >
          {label}
        </button>
      );
    }
  }
  return <span className={cls}>{label}</span>;
}

export default function SlateCard({
  card,
  codeUrl,
  kb,
  repo,
  mobile = false,
  isOperator = false,
  focused = false,
  flashing = false,
  actions = {},
}: Props) {
  const [openBody, setOpenBody] = useState(false);
  const kind = glyphForKind(card.kind);
  const who = authorChip(card.who);
  const isYou = card.who.origin === "human";
  const live = card.liveness ? glyphForLiveness(card.liveness) : null;
  const opacity = opacityForAge(card.age_secs);
  const refs = card.refs ?? [];
  const seen = seenChipFor(card);
  const rawRefs = useMemo(
    () => (card.refs ?? []).map((r) => r.raw),
    [card.refs],
  );
  // D29 — captions only on knowledge cards, and only when a `code_url`
  // exists. `useSlateGrounding` is called UNCONDITIONALLY (hook rules); it
  // fetches nothing when either half is missing.
  const grounding = useSlateGrounding(
    captionsOn(card.kind) ? codeUrl : null,
    kb,
    repo,
    rawRefs,
    card.line,
  );
  const marksTitle = useMemo(
    () =>
      card.marks_by && card.marks_by.length > 0
        ? `marked by ${card.marks_by.join(", ")}`
        : `${card.marks} mark${card.marks === 1 ? "" : "s"}`,
    [card.marks, card.marks_by],
  );

  const cls = [
    "slate-card",
    `slate-card--${card.tier}`,
    `slate-card--${card.kind}`,
    card.pinned ? "is-pinned" : "",
    focused ? "is-focused" : "",
    flashing ? "is-flashing" : "",
    card.contested ? "is-contested" : "",
  ]
    .filter(Boolean)
    .join(" ");

  return (
    <article
      className={cls}
      id={`slate-post-${card.seq}`}
      data-seq={card.seq}
      data-kind={card.kind}
      data-tier={card.tier}
      aria-labelledby={`slate-post-${card.seq}-line`}
      style={{ opacity, "--slate-kind": colorForKind(card.kind) } as React.CSSProperties}
    >
      <header className="slate-card__head">
        {/* The kind WORD is first in reading order; the glyph beside it is
            decorative (§10 "Every glyph has a text alternative"). */}
        <span className="slate-card__kind">
          <span className="slate-card__glyph" aria-hidden="true">
            {kind.glyph}
          </span>
          <span className="slate-card__kindword">{kind.word}</span>
        </span>
        <span className="slate-card__seq">#{card.seq}</span>
        {card.pinned && (
          <span className="slate-card__badge slate-card__badge--pin">
            <span aria-hidden="true">{STATE_GLYPHS.pin.glyph}</span>
            <span className="slate-card__badge-word">{STATE_GLYPHS.pin.word}</span>
          </span>
        )}
        <span className={`slate-card__who${isYou ? " is-you" : ""}`} title={card.who.tag}>
          {isYou ? `[${who}]` : who}
        </span>
        <span className="slate-card__age" title={`${card.age_secs}s`}>
          {ageLabel(card.age_secs)}
        </span>
        {card.marks > 0 && (
          <span className="slate-card__marks" title={marksTitle}>
            <span className="slate-card__ring" aria-hidden="true" />
            <span className="slate-card__marks-n">+{card.marks}</span>
          </span>
        )}
        {seen && (
          <span className="slate-card__seen" title={seen.title}>
            {seen.label}
          </span>
        )}
        {live && (
          <span className={`slate-card__badge slate-card__badge--${card.liveness}`}>
            <span aria-hidden="true">{live.glyph}</span>
            <span className="slate-card__badge-word">{live.word}</span>
          </span>
        )}
        {card.contested && (
          <span className="slate-card__badge slate-card__badge--contested">
            <span aria-hidden="true">{STATE_GLYPHS.contested.glyph}</span>
            <span className="slate-card__badge-word">{STATE_GLYPHS.contested.word}</span>
          </span>
        )}
        {card.acknowledged === false && (
          <span className="slate-card__badge slate-card__badge--unack">
            UNACKNOWLEDGED
          </span>
        )}
      </header>

      <p className="slate-card__line" id={`slate-post-${card.seq}-line`}>
        {card.line}
      </p>

      <div className="slate-card__meta">
        {card.subject && (
          <span className="slate-card__subject" title="subject">
            {card.subject}
          </span>
        )}
        {card.topic && <span className="slate-card__topic">{card.topic}</span>}
        {card.was !== undefined && (
          <button
            type="button"
            className="slate-card__was"
            onClick={() => {
              actions.onOpenHistoryAt?.(card.was!);
              actions.onJumpToSeq?.(card.was!);
            }}
          >
            (was #{card.was})
          </button>
        )}
        {card.taken_over_by && (
          <span className="slate-card__takenover">
            taken over by {card.taken_over_by}
          </span>
        )}
        {card.answers !== undefined && card.answers > 0 && (
          <span className="slate-card__answers">
            {card.answers} answer{card.answers === 1 ? "" : "s"}
          </span>
        )}
        {refs.length > 0 && (
          <span className="slate-card__refs">
            {refs.map((r) => (
              <RefChip
                key={r.raw}
                r={r}
                codeUrl={codeUrl}
                caption={grounding.get(r.raw)}
                onJumpToSeq={actions.onJumpToSeq}
              />
            ))}
          </span>
        )}
      </div>

      {card.body && (
        <>
          <button
            type="button"
            className="slate-card__bodytoggle"
            aria-expanded={openBody}
            aria-controls={`slate-post-${card.seq}-body`}
            onClick={() => setOpenBody((v) => !v)}
          >
            <Icon.ChevDown />
            {openBody ? "hide body" : card.has_sketch ? "show body + sketch" : "show body"}
          </button>
          {openBody && (
            <div className="slate-card__body" id={`slate-post-${card.seq}-body`}>
              {/* The EXISTING react-markdown pipeline (remark-gfm +
                  remark-breaks, no rehype-raw); `sketchSeq` is what turns a
                  ```mermaid fence into the sandboxed frame. */}
              <CommentBody body={card.body} className="slate-card__md" sketchSeq={card.seq} />
            </div>
          )}
        </>
      )}

      {/* Actions (§10). MOBILE is mark / drop / done only — edit and pin are
          desktop-only, so the phone stays "a reader with a capture slot"
          (rules matrix "Mobile"). `done` is offered on the three kinds that
          can be CLOSED (take, ask, hand); the daemon's re-target matrix is
          wider, but offering `done` on a found would be a button whose
          meaning nobody can state. */}
      <footer className="slate-card__acts">
        {actions.onMark && (
          <button
            type="button"
            className="slate-card__act"
            data-act="mark"
            onClick={() => actions.onMark!(card)}
            title="Circle it — +1, once per session, never your own post"
          >
            <span aria-hidden="true">{STATE_GLYPHS.mark.glyph}</span> mark
          </button>
        )}
        {actions.onDrop && (
          <button
            type="button"
            className="slate-card__act"
            data-act="drop"
            onClick={() => actions.onDrop!(card)}
            title="Wipe it off the board (the record keeps who and why)"
          >
            <Icon.Trash /> drop
          </button>
        )}
        {actions.onDone && ["take", "ask", "hand"].includes(card.kind) && (
          <button
            type="button"
            className="slate-card__act"
            data-act="done"
            onClick={() => actions.onDone!(card)}
          >
            <Icon.Check /> done
          </button>
        )}
        {!mobile && actions.onEdit && (
          <button
            type="button"
            className="slate-card__act"
            data-act="edit"
            onClick={() => actions.onEdit!(card)}
            title="Rub out and rewrite — a new post that supersedes this one"
          >
            <Icon.Pen /> edit
          </button>
        )}
        {!mobile && isOperator && actions.onPin && (
          <button
            type="button"
            className="slate-card__act"
            data-act="pin"
            aria-pressed={card.pinned}
            onClick={() => actions.onPin!(card, !card.pinned)}
          >
            <Icon.Pin /> {card.pinned ? "unpin" : "pin"}
          </button>
        )}
        {!mobile && actions.onAnswer && card.kind === "ask" && (
          <button
            type="button"
            className="slate-card__act"
            data-act="answer"
            onClick={() => actions.onAnswer!(card)}
          >
            <span aria-hidden="true">{glyphForKind("answer").glyph}</span> answer
          </button>
        )}
        {!mobile && actions.onTake && card.kind === "hand" && (
          <button
            type="button"
            className="slate-card__act"
            data-act="take"
            onClick={() => actions.onTake!(card)}
            title="Accept the handoff — copies the subject and acknowledges the hand"
          >
            <span aria-hidden="true">{glyphForKind("take").glyph}</span> take
          </button>
        )}
      </footer>
    </article>
  );
}
