// `kbc-review/1` REF CARD — one `[[…]]` reference, live (V73-K2b, design
// D9-a: "the agent's refs become live, syntax-highlighted code cards rendered
// by kb-code").
//
// Everything on this card came off the wire. The state (`pinned` / `carried`
// / `orphan` / `inert`), the trust tier, the snippet, its highlight spans and
// the caption are all the daemon's; this component decides layout and
// nothing else. Three rules it must keep:
//
// **An orphan is a VISIBLE card that says "no honest match".** It is never
// hidden and never given a guessed line — `review_doc::cards` deliberately
// does not report a position for one, and inventing one here would be the
// wrong-`exact` failure the crate's oracle bar names as a release blocker.
//
// **Highlighting is the server's.** `card.highlights` are byte offsets
// already REBASED onto `card.snippet`; `lib/diffHighlight.ts`'s
// `buildLineSpans`/`paintLine` turn them into the same `.kbc-hl-*` segments
// the diff and the dossier paint. A client-side highlighter would be a
// second, disagreeing source of truth (`GET /api/file`'s own rule), so an
// unindexed blob simply renders unpainted.
//
// **Trust is LINE STYLE.** The tier rides the shared `TrustBadge`
// (kbc-theme/1's Lane Budget) and never a hue this feature picks.
import { useMemo, type ReactNode } from "react";
import { Link } from "react-router-dom";
import type { ReviewDocCard } from "../../api/types";
import { Icon } from "../icons";
import TrustBadge from "../TrustBadge";
import { buildLineSpans, paintLine, type LineSpan } from "../../lib/diffHighlight";
import {
  cardAddress,
  cardHref,
  cardStateClass,
  cardStateLabel,
  type RefSpan,
} from "../../lib/reviewDoc";
import { codeUrl, findingUrl, reviewDiffHref } from "../../lib/codeUrl";

/** The URL builders `cardHref` needs, in one bag (root CLAUDE.md #35). */
const HREF_BUILDERS = {
  codeUrl: (loc: { repo: string; path: string; line?: { start: number; end: number } | number }) =>
    codeUrl(loc),
  reviewDiffHref: (repo: string, id: number, file?: string) => reviewDiffHref(repo, id, file),
  findingUrl,
};

/** The href a card's address links to, or `null` for an inert/orphan card. */
export function refCardHref(card: ReviewDocCard, repo: string, reviewId: number): string | null {
  return cardHref(card, HREF_BUILDERS, repo, reviewId);
}

/**
 * The one-line title a folded card shows: the scheme, then the address.
 * A folded card must still name what it points at — folding is a layout
 * choice, never a way to stop saying something.
 */
export function refCardSummary(card: ReviewDocCard): string {
  return `${card.scheme}: ${cardAddress(card)}`;
}

/** The snippet's lines paired with their real line numbers, or `[]`. */
export function snippetLines(card: ReviewDocCard): { n: number | null; text: string }[] {
  if (!card.snippet) return [];
  const start = card.snippet_start ?? null;
  return card.snippet.split("\n").map((text, i) => ({
    n: start == null ? null : start + i,
    text,
  }));
}

function PaintedLine({ text, spans }: { text: string; spans: LineSpan[] | undefined }) {
  const segs = paintLine(text, spans);
  if (segs.length === 1 && !segs[0].cls) return <>{text}</>;
  return (
    <>
      {segs.map((s, i) =>
        s.cls ? (
          <span key={i} className={s.cls} data-kbc-hl>
            {s.text}
          </span>
        ) : (
          <span key={i}>{s.text}</span>
        ),
      )}
    </>
  );
}

export interface RefCardProps {
  span: RefSpan;
  repo: string;
  reviewId: number;
  /** Folded cards show their summary line only (`?cards=folded`). */
  folded: boolean;
  /** `true` while this card is the rail's focused jump target. */
  focused?: boolean;
  onToggleFold?: (ref: string) => void;
}

/**
 * One inline reference. The four `RefSpan` kinds each render differently and
 * all of them render — a span this side cannot resolve is a visible chip
 * naming why, never absent text.
 */
export default function RefCard({
  span,
  repo,
  reviewId,
  folded,
  focused,
  onToggleFold,
}: RefCardProps) {
  if (span.kind === "malformed") {
    return (
      <span
        className="kbc-refcard kbc-refcard--malformed"
        data-kbc-refcard={span.body}
        data-kbc-refcard-state="malformed"
        title={span.reason}
      >
        <Icon.Warn />
        <code>[[{span.body}]]</code>
        <span className="kbc-refcard__why">{span.reason}</span>
      </span>
    );
  }
  if (span.kind === "unresolved") {
    return (
      <span
        className="kbc-refcard kbc-refcard--unresolved"
        data-kbc-refcard={span.body}
        data-kbc-refcard-state="unresolved"
        title={span.reason}
      >
        <Icon.Unlink />
        <code>[[{span.body}]]</code>
        <span className="kbc-refcard__why">{span.reason}</span>
      </span>
    );
  }
  if (span.kind === "wikilink") {
    // Root invariant #29 owns this syntax; it is kb's link, not ours, and it
    // reaches the reader as the author typed it.
    return <>[[{span.body}]]</>;
  }
  return (
    <LiveRefCard
      card={span.card}
      repo={repo}
      reviewId={reviewId}
      folded={folded}
      focused={focused}
      onToggleFold={onToggleFold}
    />
  );
}

/**
 * The RESOLVED card — the live snippet, its state badge, its trust tier and
 * its address — split out and exported by V74-L2 so `kbc-canvas/1` boards
 * render code nodes through THIS component rather than a second one that
 * could disagree about what `carried` looks like (D10's "cards carry the
 * reader's link affordances"). The two props it gained are both optional and
 * both default to the review document's own behaviour, so this file's
 * pre-V74 render is byte-identical:
 *
 * - `href` — `undefined` (the default) derives the link exactly as before;
 *   passing `null` or a string overrides it, which is what lets a board card
 *   link into the READER while a review card links into the review.
 * - `orphanNote` — the sentence an orphan carries. The review document's
 *   own wording names "this patchset"; a board has no patchset, and a card
 *   that borrowed the wrong sentence would be honest about the wrong thing.
 */
export function LiveRefCard({
  card,
  repo,
  reviewId,
  folded,
  focused,
  onToggleFold,
  href: hrefOverride,
  orphanNote,
}: {
  card: ReviewDocCard;
  repo: string;
  reviewId: number;
  folded: boolean;
  focused?: boolean;
  onToggleFold?: (ref: string) => void;
  href?: string | null;
  orphanNote?: ReactNode;
}) {
  const href = hrefOverride !== undefined ? hrefOverride : refCardHref(card, repo, reviewId);
  const lines = useMemo(() => snippetLines(card), [card]);
  const spansByLine = useMemo(
    () =>
      card.snippet && card.highlights && card.highlights.length > 0
        ? buildLineSpans(card.snippet, card.highlights)
        : null,
    [card.snippet, card.highlights],
  );
  const address = cardAddress(card);
  return (
    <span
      className={
        "kbc-refcard " +
        cardStateClass(card) +
        (folded ? " kbc-refcard--folded" : "") +
        (focused ? " is-focused" : "")
      }
      data-kbc-refcard={card.ref}
      data-kbc-refcard-state={card.state}
      data-kbc-refcard-scheme={card.scheme}
    >
      <span className="kbc-refcard__head">
        <button
          type="button"
          className="kbc-refcard__fold"
          aria-expanded={!folded}
          aria-label={folded ? `expand ${card.ref}` : `fold ${card.ref}`}
          onClick={() => onToggleFold?.(card.ref)}
          data-kbc-refcard-fold={card.ref}
        >
          {folded ? <Icon.Expand /> : <Icon.Collapse />}
        </button>
        <span className="kbc-refcard__scheme" data-kbc-refcard-scheme-label>
          {card.scheme}
        </span>
        {href ? (
          <Link className="kbc-refcard__addr" to={href} data-kbc-refcard-link={card.ref}>
            {address}
          </Link>
        ) : (
          <span className="kbc-refcard__addr kbc-refcard__addr--inert" data-kbc-refcard-addr>
            {address}
          </span>
        )}
        <span className="kbc-refcard__state" data-kbc-refcard-state-label={card.state}>
          {cardStateLabel(card)}
        </span>
        {/* Trust is absent for an orphan (nothing to grade) and for an inert
            link (no claim is made) — `TrustBadge` would classify a missing
            class DOWN to `candidate`, which would be a claim. So the badge
            only appears when the daemon actually minted a tier. */}
        {card.trust ? <TrustBadge cls={card.trust} title={card.caption} /> : null}
      </span>
      {/* The caption is ALWAYS shown, in every state: why it is pinned, how
          it was carried, or why there is no honest match. It is the daemon's
          sentence, rendered verbatim. */}
      <span className="kbc-refcard__caption" data-kbc-refcard-caption>
        {card.caption}
      </span>
      {!folded && lines.length > 0 && (
        <span className="kbc-refcard__code" data-kbc-refcard-snippet={card.ref}>
          {lines.map((l, i) => (
            <span className="kbc-refcard__line" key={i}>
              <span className="kbc-refcard__gutter">{l.n ?? ""}</span>
              <span className="kbc-refcard__text">
                <PaintedLine text={l.text} spans={spansByLine?.get(i + 1)} />
              </span>
            </span>
          ))}
        </span>
      )}
      {!folded && card.state === "orphan" && (
        <span className="kbc-refcard__orphan" data-kbc-refcard-orphan>
          {orphanNote ?? (
            <>
              The author wrote <code>[[{card.ref}]]</code>. Nothing in this patchset matches it
              honestly, so no position is reported — a guessed line would be worse than this hole.
            </>
          )}
        </span>
      )}
    </span>
  );
}
