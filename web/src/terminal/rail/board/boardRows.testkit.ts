// RAILBOARD — synthetic slate board fixtures shared by the Board tab's tests.

import type { SlateBoardCard, SlateBoardResponse, SlateKind, SlateWho } from "../../../api/slateTypes";

export const SID = "8f2a1111-2222-4333-8444-555566667777"; // short "8f2a"

export function who(over: Partial<SlateWho> = {}): SlateWho {
  const base: SlateWho = { origin: "agent", harness: "claude", session_short: "zzzz", tag: "claude/zzzz" };
  return { ...base, ...over };
}

export function card(seq: number, kind: SlateKind, over: Partial<SlateBoardCard> = {}): SlateBoardCard {
  return {
    seq,
    id: `p${seq}`,
    kind,
    line: `${kind} line ${seq}`,
    who: who(),
    age_secs: 120,
    tier: "whole",
    marks: 0,
    pinned: false,
    contested: false,
    body: null,
    marks_by: [],
    has_sketch: false,
    ...over,
  };
}

export function board(over: Partial<SlateBoardResponse["sections"]> = {}, head = 9): SlateBoardResponse {
  return {
    slug: "demo",
    head_seq: head,
    topics: [],
    folded_found_idea: 0,
    sections: { now: [], warn: [], hand: [], ask: [], take: [], found_idea: [], tried: [], ...over },
  };
}
