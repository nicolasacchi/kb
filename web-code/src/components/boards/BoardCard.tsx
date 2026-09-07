// `kbc-canvas/1` — ONE card per node (V74-L2, design D10).
//
// Every node kind renders, in every state. There is no branch in which a node
// is absent from this file: an orphan is a VISIBLE card carrying its address,
// its last-known text and the daemon's own reason, because "a board that
// silently loses a card is worse than a board that admits one died"
// (`boards/mod.rs`).
//
// **A `code` node is rendered by the review document's card**, not by a second
// one — `LiveRefCard` (V73-K2b, split out for this unit) paints the snippet
// with the server's own spans, shows the state badge and the caption, and owns
// the fold. Two board-specific overrides ride its two optional props: the href
// goes to the READER (`codeUrl`, through `cardHref`'s path branch) and the
// orphan sentence is the board's, not the review's.
//
// **Nothing here computes a state, a count or a link it could not justify.**
// An `annotation`/`turn`/`bookmark` node gets no link because this SPA has no
// route that addresses one — a fabricated link would be worse than none, which
// is `cardHref`'s own recorded ruling for `gh:`/`kb:`.

import { useMemo, type ReactNode } from "react";
import { Link } from "react-router-dom";
import type { BoardNode } from "../../api/types";
import { Icon } from "../icons";
import { LiveRefCard } from "../reviews/RefCard";
import {
  boardCaption,
  boardReasonLabel,
  boardStateLabel,
  cardKindFor,
  codeCardFor,
  contextCardFor,
  hasContextExpansion,
} from "../../lib/boards";
import { codeUrl, findingUrl, reviewDiffHref } from "../../lib/codeUrl";
import { parseMarkdownLite } from "../../lib/markdownLite";

export interface BoardCardProps {
  node: BoardNode;
  repo: string;
  folded: boolean;
  /// `true` when the card's `±N` context expansion is showing. Only ever true
  /// for a node whose daemon read actually carried one.
  expanded: boolean;
  focused: boolean;
  onToggleFold: (id: string) => void;
  onThread: (node: BoardNode) => void;
  /// Present only for a loopback caller — pinning writes through `apply`.
  onPin?: (node: BoardNode) => void;
  onUnpin?: (node: BoardNode) => void;
}

/// The head every NON-code card shares: the address (linked when there is an
/// honest destination), the state badge and the daemon's caption. Deliberately
/// mirrors `LiveRefCard`'s head so the two read as one family, and deliberately
/// NOT the same component: a presence card has no snippet, no highlight spans
/// and no trust tier to place.
function AddressHead({
  node,
  href,
  folded,
  onToggleFold,
}: {
  node: BoardNode;
  href: string | null;
  folded: boolean;
  onToggleFold: (id: string) => void;
}) {
  return (
    <span className="kbc-refcard__head">
      <button
        type="button"
        className="kbc-refcard__fold"
        aria-expanded={!folded}
        aria-label={folded ? `expand ${node.id}` : `fold ${node.id}`}
        onClick={() => onToggleFold(node.id)}
        data-kbc-board-fold={node.id}
      >
        {folded ? <Icon.Expand /> : <Icon.Collapse />}
      </button>
      <span className="kbc-refcard__scheme" data-kbc-board-kind-label>
        {node.kind}
      </span>
      {href ? (
        <Link className="kbc-refcard__addr" to={href} data-kbc-board-link={node.id}>
          {node.address}
        </Link>
      ) : (
        <span className="kbc-refcard__addr kbc-refcard__addr--inert" data-kbc-board-addr>
          {node.address}
        </span>
      )}
      <span className="kbc-refcard__state" data-kbc-board-state-label={node.state}>
        {boardStateLabel(node.state)}
      </span>
      <span className="kbc-refcard__caption" data-kbc-board-caption>
        {boardReasonLabel(node.reason)}
      </span>
    </span>
  );
}

/// A `note`/`group` body: authored Markdown, through the SPA's ONE Markdown
/// path. Nothing here builds an HTML string — every run is a React text child,
/// which React escapes (`DocMarkdown`'s own structural-safety rule).
function NoteBody({ source }: { source: string }) {
  const blocks = useMemo(
    () => parseMarkdownLite(source, { headings: true, fences: true }),
    [source],
  );
  return (
    <div className="kbc-boardcard__note-md" data-kbc-board-note-md>
      {blocks.map((b, i) => {
        if (b.kind === "code") {
          return (
            <pre className="kbc-doc__fence" key={i}>
              <code>{b.text}</code>
            </pre>
          );
        }
        if (b.kind === "list") {
          return (
            <ul key={i}>
              {b.items.map((item, j) => (
                <li key={j}>{item.map((r, k) => <Run key={k} kind={r.kind} text={r.text} />)}</li>
              ))}
            </ul>
          );
        }
        if (b.kind === "heading") {
          const level = Math.min(6, b.level + 2);
          const Tag = `h${level}` as "h3" | "h4" | "h5" | "h6";
          return (
            <Tag key={i} className="kbc-doc__heading">
              {b.runs.map((r, k) => <Run key={k} kind={r.kind} text={r.text} />)}
            </Tag>
          );
        }
        return <p key={i}>{b.runs.map((r, k) => <Run key={k} kind={r.kind} text={r.text} />)}</p>;
      })}
    </div>
  );
}

function Run({ kind, text }: { kind: string; text: string }) {
  if (kind === "bold") return <b>{text}</b>;
  if (kind === "code") return <code>{text}</code>;
  return <span>{text}</span>;
}

/// The `query` card. Every number is the daemon's: `authored_count` is what the
/// author saw, `current_count` arrives ONLY on a live read, `basis` says the
/// count is over a page, and `delta` is the daemon's subtraction. A count this
/// side computed would be the second, disagreeing source of truth the whole
/// wire exists to prevent.
function QueryBody({ node }: { node: BoardNode }) {
  const card = node.query_card;
  if (!card) return null;
  const delta = card.delta;
  return (
    <div className="kbc-boardcard__query" data-kbc-board-query>
      <code className="kbc-boardcard__kbcq" data-kbc-board-kbcq>
        {card.query}
      </code>
      <span className="kbc-boardcard__counts">
        {card.authored_count != null && (
          <span data-kbc-board-authored-count>{card.authored_count} when authored</span>
        )}
        {card.current_count != null ? (
          <span data-kbc-board-current-count>
            {card.current_count} now
            {card.basis ? ` (${card.basis} count)` : ""}
            {card.truncated ? " · a lane reported more" : ""}
          </span>
        ) : (
          <span data-kbc-board-count-absent>count not re-run on this read</span>
        )}
        {delta != null && delta !== 0 && (
          <span data-kbc-board-delta={String(delta)}>
            {delta > 0 ? `+${delta}` : delta} since authored
          </span>
        )}
      </span>
    </div>
  );
}

/// Where a card's address points, or `null` when nothing honest can be built.
/// Four destinations and five refusals, each with a reason in the doc above.
export function boardCardHref(node: BoardNode, repo: string): string | null {
  if (node.state === "orphan") return null;
  switch (node.kind) {
    case "code":
      if (!node.code) return null;
      return codeUrl({
        repo,
        path: node.code.path,
        line: { start: node.code.range[0], end: node.code.range[1] },
      });
    case "hunk":
      // The review diff at that FILE, never at the hunk: `?hunk=` takes a
      // `kbc-hunkid/1` content address computed in the browser from a parsed
      // diff, which a board node does not carry (`cardHref`'s own ruling).
      return node.review != null
        ? reviewDiffHref(repo, node.review, node.path ?? undefined)
        : null;
    case "finding":
      return node.review != null && node.finding
        ? findingUrl(repo, node.review, node.finding)
        : null;
    case "link":
      return node.url ?? null;
    default:
      // `annotation` / `turn` / `bookmark` / `note` / `group`: this SPA has no
      // route that addresses one of them on its own.
      return null;
  }
}

export default function BoardCard({
  node,
  repo,
  folded,
  expanded,
  focused,
  onToggleFold,
  onThread,
  onPin,
  onUnpin,
}: BoardCardProps) {
  const kind = cardKindFor(node);
  const href = boardCardHref(node, repo);
  const codeCard = codeCardFor(node);
  const contextCard = contextCardFor(node);
  const shownCard = expanded && contextCard ? contextCard : codeCard;

  let body: ReactNode = null;
  if (kind === "code" && shownCard) {
    body = (
      <LiveRefCard
        card={shownCard}
        repo={repo}
        reviewId={0}
        folded={folded}
        focused={focused}
        onToggleFold={() => onToggleFold(node.id)}
        href={href}
        orphanNote={
          <>
            This card points at <code>{node.address}</code>. Nothing in the working tree matches
            it honestly, so no position is reported — the card keeps its last-known text and says
            the code is gone.
          </>
        }
      />
    );
  } else {
    body = (
      <>
        <AddressHead node={node} href={href} folded={folded} onToggleFold={onToggleFold} />
        {!folded && kind === "note" && node.body_md ? <NoteBody source={node.body_md} /> : null}
        {!folded && kind === "group" ? (
          <div className="kbc-boardcard__group" data-kbc-board-group>
            {node.members?.length ? `${node.members.length} member(s)` : "an empty group"}
            {node.body_md ? <NoteBody source={node.body_md} /> : null}
          </div>
        ) : null}
        {!folded && kind === "query" ? <QueryBody node={node} /> : null}
        {!folded && kind === "link" && node.url ? (
          <a
            className="kbc-boardcard__url"
            href={node.url}
            target="_blank"
            rel="noreferrer noopener"
            data-kbc-board-url
          >
            {node.url}
          </a>
        ) : null}
      </>
    );
  }

  return (
    <article
      className={`kbc-boardcard kbc-boardcard--${kind}${focused ? " is-focused" : ""}${folded ? " kbc-boardcard--folded" : ""}`}
      data-kbc-board-node={node.id}
      data-kbc-board-kind={node.kind}
      data-kbc-board-state={node.state}
      data-kbc-board-reason={node.reason}
      data-kbc-board-pinned={node.pin ? "1" : undefined}
      tabIndex={-1}
      aria-label={`${node.kind} card ${node.id} — ${boardStateLabel(node.state)}`}
    >
      <header className="kbc-boardcard__head">
        <span className="kbc-boardcard__title" data-kbc-board-title>
          {node.title ?? node.id}
        </span>
        <span className="kbc-boardcard__actions">
          {hasContextExpansion(node) && (
            <span className="kbc-boardcard__ctx" data-kbc-board-ctx={expanded ? "wide" : "narrow"}>
              {expanded ? "± context" : "primary"}
            </span>
          )}
          {node.pin && (
            <span className="kbc-boardcard__pinned" title="pinned by hand" data-kbc-board-pin>
              <Icon.Pin />
            </span>
          )}
          <button
            type="button"
            className="kbc-boardcard__thread"
            onClick={() => onThread(node)}
            data-kbc-board-thread={node.id}
            aria-label={`thread on ${node.id}`}
            title="Open this node's thread (an annotation, in the annotations store)"
          >
            <Icon.Comment />
            {node.thread ? (
              <span data-kbc-board-thread-count>{node.thread.replies}</span>
            ) : null}
          </button>
          {onPin && !node.pin && (
            <button
              type="button"
              className="kbc-boardcard__pin-btn"
              onClick={() => onPin(node)}
              data-kbc-board-pin-here={node.id}
              title="Pin this card here (writes a pin through apply — loopback only)"
              aria-label={`pin ${node.id} here`}
            >
              <Icon.Pin />
            </button>
          )}
          {onUnpin && node.pin && (
            <button
              type="button"
              className="kbc-boardcard__pin-btn"
              onClick={() => onUnpin(node)}
              data-kbc-board-unpin={node.id}
              title="Release this pin — the layout engine places the card again"
              aria-label={`unpin ${node.id}`}
            >
              <Icon.X />
            </button>
          )}
        </span>
      </header>
      <div className="kbc-boardcard__body">{body}</div>
      {node.note ? (
        <p className="kbc-boardcard__note" data-kbc-board-note>
          {node.note}
        </p>
      ) : null}
      {node.code?.snippet_truncated ? (
        <p className="kbc-boardcard__note" data-kbc-board-truncated>
          this range is longer than the snippet cap — the card shows its first lines, not all of it
        </p>
      ) : null}
      {kind !== "code" && node.reason ? (
        <p className="kbc-boardcard__reason" data-kbc-board-reason-label>
          {boardCaption(node)}
        </p>
      ) : null}
    </article>
  );
}
