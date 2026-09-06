// The board's PURE layout engine: grouping, ordering, tiering and the
// attention count. No React, no DOM, no fetch — every function here is a
// total function of the projection the daemon already computed.
//
// THE RULE (design §10 "Layout and rendering"): "Pinned cards sit first in
// their column, then cards with marks (descending), then newest first: the
// same order the digest uses, so the two never disagree." That order is
// implemented ONCE, in `orderCards`, and every column, swimlane and NOW row
// goes through it.
//
// The board's FIVE columns are a view over the projection's SEVEN sections
// (NOW and WARN are the band, not a column; FOUND and IDEA already share a
// section server-side). Nothing is re-derived from `kind` — the daemon owns
// section membership, and re-deriving it here is exactly how a second,
// disagreeing classifier gets born.

import type {
  SlateBoardCard,
  SlateBoardSections,
  SlateCounts,
  SlateKind,
  SlateSummary,
  SlateTier,
} from "../api/slateTypes";

/// The five column ids, in reading order.
export type SlateColumnId = "hand" | "ask" | "take" | "found_idea" | "tried";

export const COLUMN_IDS: readonly SlateColumnId[] = [
  "hand",
  "ask",
  "take",
  "found_idea",
  "tried",
];

/// The `role="region"` accessible name for each column (§10
/// "Accessibility": columns are regions WITH the section name).
export const COLUMN_NAMES: Readonly<Record<SlateColumnId, string>> = {
  hand: "Hands",
  ask: "Asks",
  take: "Takes",
  found_idea: "Found and ideas",
  tried: "Tried",
};

/// The general lane's label — the digest renders it `NOW  —`, so the board
/// spells the same absence out rather than inventing a topic name.
export const GENERAL_LANE = "—";

export type SlateColumn = {
  id: SlateColumnId;
  name: string;
  cards: SlateBoardCard[];
};

/// Pinned first, then marks descending, then newest first (highest seq).
/// STABLE and total: equal keys keep their server order, which is already
/// the digest's. Never mutates the input.
export function orderCards(cards: readonly SlateBoardCard[]): SlateBoardCard[] {
  return [...cards].sort((a, b) => {
    if (a.pinned !== b.pinned) return a.pinned ? -1 : 1;
    if (a.marks !== b.marks) return b.marks - a.marks;
    return b.seq - a.seq;
  });
}

/// The five columns, each ordered. Missing sections degrade to empty — a
/// board with no takes still renders the TAKES region (an empty region is
/// an honest "nothing here", a missing one is a layout that moves).
export function columnsOf(
  sections: SlateBoardSections | undefined,
): SlateColumn[] {
  return COLUMN_IDS.map((id) => ({
    id,
    name: COLUMN_NAMES[id],
    cards: orderCards(sections?.[id] ?? []),
  }));
}

// ── swimlanes ────────────────────────────────────────────────────────────

export type SlateLane = {
  /// The authored topic, or null for the general lane.
  topic: string | null;
  /// What the lane header renders (`GENERAL_LANE` when topic is null).
  label: string;
  columns: SlateColumn[];
  /// Cards in this lane across every column — drives the lane's count and
  /// the mobile accordion.
  total: number;
};

/// One row per topic plus the general lane, each holding the same five
/// columns. Lane order: the topics in the order the board reported them
/// (the daemon's own, which is the digest's), then any topic that only
/// appears on a card, then the general lane LAST — an untopicked post is
/// the least specific thing on the board.
export function lanesOf(
  sections: SlateBoardSections | undefined,
  topics: readonly string[] = [],
): SlateLane[] {
  const cols = columnsOf(sections);
  const seen: string[] = [];
  const push = (t: string) => {
    if (t && !seen.includes(t)) seen.push(t);
  };
  topics.forEach(push);
  for (const c of cols) for (const card of c.cards) if (card.topic) push(card.topic);

  const laneFor = (topic: string | null): SlateLane => {
    const columns = cols.map((c) => ({
      ...c,
      cards: c.cards.filter((card) => (card.topic ?? null) === topic),
    }));
    return {
      topic,
      label: topic ?? GENERAL_LANE,
      columns,
      total: columns.reduce((n, c) => n + c.cards.length, 0),
    };
  };

  const lanes = seen.map((t) => laneFor(t));
  const general = laneFor(null);
  if (general.total > 0 || lanes.length === 0) lanes.push(general);
  return lanes;
}

// ── the NOW band ─────────────────────────────────────────────────────────

export type NowRow = {
  topic: string | null;
  label: string;
  card: SlateBoardCard;
};

/// The band's NOW rows: the newest live `now` per topic, in the board's
/// topic order with the general lane last. Derived from the `now` section,
/// NOT from `header.topics` — the board view returns cards (with bodies and
/// marks), the digest header returns `Projected`s.
export function nowRows(
  sections: SlateBoardSections | undefined,
  topics: readonly string[] = [],
): NowRow[] {
  const cards = orderCards(sections?.now ?? []);
  const order = new Map<string, number>();
  topics.forEach((t, i) => order.set(t, i));
  const rows = cards.map((card) => ({
    topic: card.topic ?? null,
    label: card.topic ?? GENERAL_LANE,
    card,
  }));
  return rows.sort((a, b) => {
    const ai = a.topic === null ? Number.MAX_SAFE_INTEGER : (order.get(a.topic) ?? Number.MAX_SAFE_INTEGER - 1);
    const bi = b.topic === null ? Number.MAX_SAFE_INTEGER : (order.get(b.topic) ?? Number.MAX_SAFE_INTEGER - 1);
    if (ai !== bi) return ai - bi;
    return b.card.seq - a.card.seq;
  });
}

/// The band's WARN rows — ordered, never truncated, never faded away: a
/// warn "never ages" (rules matrix "Warn lifetime"); age is only displayed.
export function warnRows(
  sections: SlateBoardSections | undefined,
): SlateBoardCard[] {
  return orderCards(sections?.warn ?? []);
}

// ── the attention count ──────────────────────────────────────────────────

/// The board chip: "unacknowledged hands + open asks + contested takes +
/// stale takes", summed CLIENT-side off `GET /api/slates`'s per-section
/// counts (§9: "the board chip = hand_unack + ask_open + take_contested +
/// take_stale, summed client-side"). The daemon reports counts; it never
/// reports an attention SCORE, and this must stay the only place the four
/// terms are added — a second copy is a second definition.
export function attentionCount(counts: SlateCounts | undefined): number {
  if (!counts) return 0;
  return (
    (counts.hand_unack || 0) +
    (counts.ask_open || 0) +
    (counts.take_contested || 0) +
    (counts.take_stale || 0)
  );
}

/// Fleet-wide attention across every slate — what the nav badge shows.
/// Hidden at zero by the caller; this returns the honest 0 rather than
/// null, so the "never flashes 0" rule lives in ONE place (the render
/// guard), not in two disagreeing ones.
export function totalAttention(slates: readonly SlateSummary[] | undefined): number {
  if (!slates) return 0;
  return slates.reduce((n, s) => n + attentionCount(s.counts), 0);
}

/// Rank for the `/slates` list: most attention first, then most recently
/// updated, then slug. Closed slates sink below every open one — a closed
/// slate is a record, not a surface that can change your next action.
export function orderSlates(
  slates: readonly SlateSummary[] | undefined,
): SlateSummary[] {
  return [...(slates ?? [])].sort((a, b) => {
    if (a.closed !== b.closed) return a.closed ? 1 : -1;
    const d = attentionCount(b.counts) - attentionCount(a.counts);
    if (d !== 0) return d;
    if (a.updated_unix !== b.updated_unix) return b.updated_unix - a.updated_unix;
    return a.slug.localeCompare(b.slug);
  });
}

// ── reading order (the j/k cursor) ───────────────────────────────────────

/// Every card the board renders, in READING order: the NOW band (now rows
/// then warn rows) and then each column top to bottom, left to right. This
/// is what `j`/`k` walks, and — because §10 pins "cards in reading order" —
/// it is also the DOM order the route emits, so the keyboard cursor and the
/// tab order can never disagree.
export function readingOrder(
  sections: SlateBoardSections | undefined,
  topics: readonly string[] = [],
  swimlanes = false,
): SlateBoardCard[] {
  const out: SlateBoardCard[] = [];
  for (const r of nowRows(sections, topics)) out.push(r.card);
  for (const w of warnRows(sections)) out.push(w);
  if (swimlanes) {
    for (const lane of lanesOf(sections, topics))
      for (const c of lane.columns) out.push(...c.cards);
  } else {
    for (const c of columnsOf(sections)) out.push(...c.cards);
  }
  return out;
}

/// Move the cursor by `delta` within a reading order, clamped at both ends
/// (never wraps — a wrap makes `j` at the bottom look like a jump to an
/// unrelated card). `null` seq means "not yet placed": `j` starts at the
/// first card, `k` at the last.
export function moveCursor(
  order: readonly SlateBoardCard[],
  current: number | null,
  delta: number,
): number | null {
  if (order.length === 0) return null;
  if (current === null) return delta > 0 ? order[0].seq : order[order.length - 1].seq;
  const i = order.findIndex((c) => c.seq === current);
  if (i < 0) return order[0].seq;
  const next = Math.min(order.length - 1, Math.max(0, i + delta));
  return order[next].seq;
}

// ── D27 · seen chips (v0.42) ─────────────────────────────────────────────

/// The three kinds whose WHOLE-tier cards carry a seen chip — NOW, HAND and
/// ASK, "the three the operator acts on" (D27; kb-core's `item_text` gates on
/// exactly these, `slate.rs`'s
/// `seen_by_renders_on_three_kinds_at_whole_tier_only`). A WARN deliberately
/// does NOT carry one, and neither does a folded card: a folded card is the
/// projection already saying "this is not what you should look at", and a
/// chip there is noise. The board matches the digest here rather than
/// inventing a wider rule, so the two presenters can never disagree about
/// what a line says.
export const SEEN_KINDS: readonly SlateKind[] = ["now", "hand", "ask"];

/// `seen by 2`, or null when nobody has been served this post.
///
/// PURE and total: an absent/empty list is null (the chip must never flash a
/// confident `seen by 0` — the same "hidden at zero" reflex as the attention
/// chip). The count is the LIST's length, so the chip and its hover can
/// never disagree; the daemon already excluded the author.
export function seenLabel(seenBy: readonly string[] | undefined | null): string | null {
  const n = seenBy?.length ?? 0;
  return n > 0 ? `seen by ${n}` : null;
}

/// The chip's hover text. D27's own words: a cursor is attribution, not
/// acknowledgement of reading — so the hover says SERVED, never "read".
export function seenTitle(seenBy: readonly string[] | undefined | null): string | undefined {
  const list = seenBy ?? [];
  return list.length > 0 ? `served to ${list.join(", ")}` : undefined;
}

/// The whole rule in one place: which cards show a chip, what it says and
/// what it says on hover. Both presenters (the NOW band's rows and
/// `SlateCard`) call THIS, so the band and the columns can never grow two
/// different answers.
export function seenChipFor(card: {
  kind: SlateKind;
  tier: SlateTier;
  seen_by?: string[] | null;
}): { label: string; title: string } | null {
  if (card.tier !== "whole") return null;
  if (!SEEN_KINDS.includes(card.kind)) return null;
  const label = seenLabel(card.seen_by);
  if (!label) return null;
  return { label, title: seenTitle(card.seen_by) ?? label };
}
