// `kbc-canvas/1` — the PURE half of the board surface (V74-L2, design D10).
//
// Every vocabulary below is a MIRROR of the daemon's own closed set
// (`crates/kb-code-server/src/boards/mod.rs` and `boards/resolve.rs`), kept in
// lock-step by `boards.vocab.test.ts`, which reads those two Rust files and
// fails naming the value that moved. That is the kbcq/1 discipline (see
// `web-code/CLAUDE.md` § Search grammar) applied to a vocabulary rather than a
// grammar: neither side generates the other, and one shared source of truth is
// what keeps them from drifting.
//
// Three rules this module exists to keep:
//
// **Nothing here computes a state, a count or a trust class.** The daemon
// re-resolves every node on every read (`boards/resolve.rs`'s module doc: "a
// stale fact must never read as a fresh one"), so this side only LABELS what
// came back. `boardStateLabel`/`boardReasonLabel` are total over the closed
// vocabularies and fall back to the raw wire string — an unknown value is
// shown, never swallowed and never guessed at.
//
// **An orphan is a card, not a gap.** `cardKindFor` returns a renderer for
// EVERY (kind, state) pair; there is no branch in which a node disappears.
//
// **A `code` node's card is the review document's card.** `codeCardFor` adapts
// a board node onto `ReviewDocCard`, the shape `components/reviews/RefCard.tsx`
// already paints (server spans, `TrustBadge`, the state badge) — so there is
// ONE live code card in this SPA, not two that could disagree about what
// `carried` looks like.

import type {
  BoardEdge,
  BoardNode,
  BoardOut,
  ReviewDocCard,
} from "../api/types";

// --- the closed vocabularies (mirrors) ------------------------------------

/// `boards::NODE_KINDS`, in the daemon's own order.
export const BOARD_NODE_KINDS = [
  "code",
  "note",
  "query",
  "hunk",
  "finding",
  "annotation",
  "turn",
  "bookmark",
  "group",
  "link",
] as const;
export type BoardNodeKind = (typeof BOARD_NODE_KINDS)[number];

/// `boards::EDGE_KINDS`. Each gets its own stroke; the SET is the contract.
export const BOARD_EDGE_KINDS = [
  "calls",
  "renders",
  "reads",
  "writes",
  "then",
  "implements",
  "contradicts",
  "question",
] as const;
export type BoardEdgeKind = (typeof BOARD_EDGE_KINDS)[number];

/// `boards::resolve::NODE_STATES`.
export const BOARD_NODE_STATES = [
  "pinned",
  "carried",
  "orphan",
  "present",
  "inert",
] as const;
export type BoardNodeState = (typeof BOARD_NODE_STATES)[number];

/// `boards::resolve::NODE_REASONS`. Every state carries one.
export const BOARD_NODE_REASONS = [
  "blob-current",
  "guard-match",
  "reanchored-exact",
  "reanchored-fuzzy",
  "path-gone",
  "content-unreadable",
  "no-anchor",
  "range-outside-file",
  "target-present",
  "target-gone",
  "no-reference",
] as const;
export type BoardNodeReason = (typeof BOARD_NODE_REASONS)[number];

/// `boards::STATUSES`. `pending` is D21's: an agent-proposed board is pending
/// until a human accepts it over loopback.
export const BOARD_STATUSES = ["pending", "draft", "accepted", "archived"] as const;
export type BoardStatus = (typeof BOARD_STATUSES)[number];

/// `boards::PROVENANCE_AUTHORED` / `PROVENANCE_DERIVED`.
export const BOARD_EDGE_PROVENANCE = ["authored", "derived"] as const;

// --- labels ---------------------------------------------------------------

/// The short word a state badge shows. Total over the wire: an unrecognised
/// state renders VERBATIM rather than being folded into a nearby one, because
/// a new daemon state read as `inert` would be a silent lie.
export function boardStateLabel(state: string): string {
  switch (state) {
    case "pinned":
      return "pinned";
    case "carried":
      return "carried";
    case "orphan":
      return "no honest match";
    case "present":
      return "present";
    case "inert":
      return "inert";
    default:
      return state;
  }
}

/// The state's CSS modifier — trust, where a node has one, rides `TrustBadge`'s
/// own LINE STYLE (kbc-theme/1's Lane Budget), never a hue this feature picks.
export function boardStateClass(state: string): string {
  return `kbc-refcard--${BOARD_NODE_STATES.includes(state as BoardNodeState) ? state : "inert"}`;
}

/// One sentence per REASON, in the daemon's own vocabulary. Unknown reasons
/// pass through verbatim (see `boardStateLabel`).
export function boardReasonLabel(reason: string): string {
  switch (reason) {
    case "blob-current":
      return "the blob is unchanged since it was written";
    case "guard-match":
      return "the blob changed, but this range's bytes still hash the same";
    case "reanchored-exact":
      return "the anchored line was found again, verbatim";
    case "reanchored-fuzzy":
      return "the anchored line was matched approximately";
    case "path-gone":
      return "the file is gone";
    case "content-unreadable":
      return "the file could not be read as text";
    case "no-anchor":
      return "nothing in the file matches the line this was anchored to";
    case "range-outside-file":
      return "the range falls outside the file";
    case "target-present":
      return "the addressed thing exists";
    case "target-gone":
      return "the addressed thing is gone";
    case "no-reference":
      return "there is nothing to resolve";
    default:
      return reason;
  }
}

/// The caption a card shows under its address: the reason, plus the SHIFT when
/// the node carried. Composed from wire values only — this side adds no claim.
export function boardCaption(node: BoardNode): string {
  const parts = [boardReasonLabel(node.reason)];
  const shift = node.code?.shifted_by ?? 0;
  if (shift !== 0) {
    const dir = shift > 0 ? "down" : "up";
    const n = Math.abs(shift);
    parts.push(`moved ${dir} ${n} line${n === 1 ? "" : "s"}`);
  }
  return parts.join(" · ");
}

// --- card selection -------------------------------------------------------

/// Which renderer a node gets. A TOTAL function over the wire's `kind`: an
/// unknown kind falls to the address card, which shows the daemon's own
/// `address`, `state` and `reason` and is therefore honest for anything.
export type BoardCardKind = "code" | "note" | "query" | "group" | "link" | "address";

export function cardKindFor(node: BoardNode): BoardCardKind {
  switch (node.kind) {
    case "code":
      return "code";
    case "note":
      return "note";
    case "query":
      return "query";
    case "group":
      return "group";
    case "link":
      return "link";
    default:
      return "address";
  }
}

/// Adapt a `code` node onto the review document's card shape so ONE component
/// paints both (see this module's header).
///
/// Two deliberate choices:
///
/// - `trust` is always absent. The daemon mints NO trust class for a board
///   node, and `trustTierFrom` classifies a missing class DOWN to `candidate`
///   — so passing one through would be a claim nobody made.
/// - `snippet_start` is the CURRENT range's first line, not the authored one.
///   A carried card's gutter must number the bytes where they ARE; the authored
///   range is still on the wire (`authored_range`) and the caption names the
///   move.
///
/// Returns `null` for a node with no `code` card — an orphan whose file is gone
/// still HAS one (with `snippet: null`), so this is genuinely "not a code node".
export function codeCardFor(node: BoardNode): ReviewDocCard | null {
  const code = node.code;
  if (!code) return null;
  return {
    ref: node.id,
    scheme: node.kind,
    state: node.state,
    trust: null,
    path: code.path,
    line: code.range[0],
    line_end: code.range[1],
    blob_sha: code.authored_blob_sha ?? null,
    current_blob: code.current_blob_sha ?? null,
    snippet: code.snippet ?? null,
    snippet_start: code.range[0],
    highlights: code.highlights ?? null,
    caption: boardCaption(node),
  };
}

/// The `±N` context expansion's card: the SAME shape, over the daemon's
/// `context_snippet` and `context` range.
///
/// It is a FETCHED expansion, never a synthesised one — `context_snippet` only
/// arrives when the read passed `ctx=1` AND the context range resolved, so a
/// card with no expansion available renders at its wire width under a caption
/// rather than inventing surrounding lines (`lib/diffContext.ts`'s own rule for
/// the review diff, applied here).
export function contextCardFor(node: BoardNode): ReviewDocCard | null {
  const code = node.code;
  if (!code || !code.context || !code.context_snippet) return null;
  return {
    ...(codeCardFor(node) as ReviewDocCard),
    line: code.context[0],
    line_end: code.context[1],
    snippet: code.context_snippet,
    snippet_start: code.context[0],
    // The daemon rebases highlight spans onto the PRIMARY snippet; they do not
    // address the context one, so the expansion renders unpainted rather than
    // painted wrong.
    highlights: null,
  };
}

/// `true` when this node has a context range the daemon could expand into —
/// what the `+` key may act on. `false` is the honest answer both when there is
/// no context range and when the read did not ask for one.
export function hasContextExpansion(node: BoardNode): boolean {
  return Boolean(node.code?.context && node.code?.context_snippet);
}

// --- edges ----------------------------------------------------------------

/// An edge's stroke. D10, verbatim: *"derived edges carry their class,
/// authored edges have one stroke."* So an AUTHORED edge gets exactly one
/// class and never a trust modifier — a human's arrow must never be able to
/// look like a SCIP-verified one (the research's "trust laundering" risk).
export function edgeClass(edge: BoardEdge): string {
  const kind = BOARD_EDGE_KINDS.includes(edge.kind as BoardEdgeKind) ? edge.kind : "then";
  const base = `kbc-board__edge kbc-board__edge--${kind}`;
  if (edge.provenance !== "derived") return `${base} kbc-board__edge--authored`;
  const trust = edge.trust ?? "candidate";
  return `${base} kbc-board__edge--derived kbc-trust-${trust}`;
}

/// The `title` an edge's stroke carries, naming its provenance out loud.
export function edgeTitle(edge: BoardEdge): string {
  const label = edge.label ? `${edge.kind} — ${edge.label}` : edge.kind;
  return edge.provenance === "derived"
    ? `${label} · derived (${edge.trust ?? "unclassed"})`
    : `${label} · authored`;
}

// --- census ---------------------------------------------------------------

/// The header strip's lines. EVERY number is `honesty`'s, verbatim; the only
/// thing this function does is decide which of them are worth a sentence.
/// A count this side computed would be a second, disagreeing source of truth
/// (kbc-tree/1's "do not re-derive a count here" rule).
export function boardCensus(board: BoardOut): string[] {
  const h = board.honesty;
  const out = [
    `${h.nodes} node${h.nodes === 1 ? "" : "s"} · ${h.edges} edge${h.edges === 1 ? "" : "s"} · ${h.steps} step${h.steps === 1 ? "" : "s"}`,
    `${h.pinned} pinned · ${h.carried} carried · ${h.present} present · ${h.inert} inert · ${h.orphans} orphan${h.orphans === 1 ? "" : "s"}`,
  ];
  if (h.orphans > 0) {
    out.push("an orphan keeps its address and its last-known text — it is shown, never dropped");
  }
  if (h.truncated_snippets > 0) {
    out.push(
      `${h.truncated_snippets} snippet${h.truncated_snippets === 1 ? " was" : "s were"} cut at the ${h.budget.max_snippet_lines}-line cap`,
    );
  }
  if (h.stale_pins > 0) {
    out.push(
      `${h.stale_pins} stale pin${h.stale_pins === 1 ? "" : "s"} — a fixed position still held for a card that no longer points anywhere`,
    );
  }
  if (!h.live_queries) {
    out.push("query counts are as authored — turn on live counts to re-run them");
  }
  return out;
}

/// The nodes in READING order: the board's own `steps` first (a walkthrough IS
/// a reading order), then everything the steps do not name, in wire order.
/// Never a sort this side invented.
export function readingOrder(board: BoardOut): BoardNode[] {
  const byId = new Map(board.nodes.map((n) => [n.id, n]));
  const out: BoardNode[] = [];
  const seen = new Set<string>();
  for (const s of board.steps) {
    const n = byId.get(s.node);
    if (n && !seen.has(n.id)) {
      seen.add(n.id);
      out.push(n);
    }
  }
  for (const n of board.nodes) {
    if (!seen.has(n.id)) {
      seen.add(n.id);
      out.push(n);
    }
  }
  return out;
}
