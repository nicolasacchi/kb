// RAILBOARD — pure shaping for the Board rail tab: split and order the slate
// board's asks, find this session's own posts, count the asks that need the
// operator, and word each section's freshness. No React, no fetching: the
// slate board projection is the only input and nothing here scores anything.

import type { SlateBoardCard, SlateBoardResponse, SlateBoardSections } from "../../../api/slateTypes";
import { ageLabel } from "../../../components/slate/SlateCard";
import { orderCards } from "../../../lib/slateLanes";

/** The server's `is_uuid_v7` (kb-core slate.rs): 36 chars, version nibble 7, dashes at 8/13/18/23, hex elsewhere. */
function isUuidV7(s: string): boolean {
  if (s.length !== 36 || s[14] !== "7") return false;
  for (let i = 0; i < 36; i++) {
    const c = s[i] as string;
    if (i === 8 || i === 13 || i === 18 || i === 23) {
      if (c !== "-") return false;
    } else if (!/^[0-9a-fA-F]$/.test(c)) return false;
  }
  return true;
}

/** Mirror of the server's `session_short`: last four chars of a UUIDv7, else the first four, `?` when empty. */
export function sessionShort(sid: string | null | undefined): string {
  if (!sid) return "?";
  if (isUuidV7(sid)) return sid.slice(-4);
  return Array.from(sid).slice(0, 4).join("");
}

/** An ask is open while nobody has answered it. */
export function isOpenAsk(c: SlateBoardCard): boolean {
  return (c.answers ?? 0) === 0;
}

export type AskGroups = {
  /** Open asks addressed to the operator. */
  forYou: SlateBoardCard[];
  /** Other open asks. */
  other: SlateBoardCard[];
  /** Answered asks (muted), operator-addressed first. */
  answered: SlateBoardCard[];
};

/** Open to-you asks first, then other open asks, then the answered ones; each group in the board's own card order. */
export function splitAsks(asks: readonly SlateBoardCard[]): AskGroups {
  const ordered = orderCards(asks);
  const open = ordered.filter(isOpenAsk);
  const done = ordered.filter((c) => !isOpenAsk(c));
  return {
    forYou: open.filter((c) => c.to === "you"),
    other: open.filter((c) => c.to !== "you"),
    answered: [...done.filter((c) => c.to === "you"), ...done.filter((c) => c.to !== "you")],
  };
}

export function flattenAsks(g: AskGroups): SlateBoardCard[] {
  return [...g.forYou, ...g.other, ...g.answered];
}

const SECTION_KEYS: readonly (keyof SlateBoardSections)[] = ["now", "warn", "hand", "ask", "take", "found_idea", "tried"];

/** This session's own agent posts across every section (never human posts; matched on the 4-char `session_short`). */
export function ownPosts(board: SlateBoardResponse, sid: string | null | undefined): SlateBoardCard[] {
  if (!sid) return [];
  const short = sessionShort(sid);
  const seen = new Set<number>();
  const out: SlateBoardCard[] = [];
  for (const k of SECTION_KEYS) {
    for (const c of board.sections[k] ?? []) {
      if (c.who.origin === "human" || c.who.session_short !== short || seen.has(c.seq)) continue;
      seen.add(c.seq);
      out.push(c);
    }
  }
  return out.sort((a, b) => b.seq - a.seq);
}

/** Open asks addressed to the operator: the only thing the tab badges. */
export function badgeCount(board: SlateBoardResponse | undefined): number {
  return (board?.sections.ask ?? []).filter((c) => c.to === "you" && isOpenAsk(c)).length;
}

/** "newest 4m ago" from the freshest card, or "no posts". */
export function freshness(cards: readonly SlateBoardCard[]): string {
  if (cards.length === 0) return "no posts";
  const age = Math.min(...cards.map((c) => c.age_secs));
  const label = ageLabel(age);
  return label ? `newest ${label} ago` : "no posts";
}

export function boardAsOf(board: SlateBoardResponse): string {
  return `board as of seq ${board.head_seq}`;
}

/** True when a slate read failed because the slate does not exist (the 404 problem), not because the daemon is unreachable. */
export function isNoSlate(error: string | null): boolean {
  return error !== null && /slate-unknown|\b404\b|not found/i.test(error);
}
