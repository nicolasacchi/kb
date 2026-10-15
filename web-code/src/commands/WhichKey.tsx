// V70-A5 — the which-key overlay (kb-code adapter).
//
// The recon's G4: press `g` in the reader and you got the character `g` in a
// status pip; there are seventeen legal continuations and no way to see them
// without leaving and pressing `?`.
//
// This is a PASSIVE OBSERVER (§P2's word). It never listens for a key, never
// dispatches, and holds no state of its own: `CommandRoot` hands it the
// pending chord and the continuations the resolver already computed, and
// `toView` maps them onto the shared, registry-agnostic overlay in
// `@kb/ui/keys/WhichKey`. Bottom-anchored and non-modal on purpose.

import { WhichKey as SharedWhichKey } from "@kb/ui/keys/WhichKey";
import type { KeyScopeView } from "@kb/ui/keys/types";
import { KBC_LEADER, type KbcCommand, type KbcScope } from "./registry.gen";
import type { ChordState } from "./dispatch";

export interface WhichKeyHint {
  command: KbcCommand;
  /// The single token this hint would consume next.
  next: string;
  /// The whole remaining sequence — more than one token means this row is a
  /// GROUP node from here, rendered with the `…` the design asks for.
  rest: string[];
}

function display(token: string): string {
  if (token === "Space") return KBC_LEADER;
  return token;
}

/// Pure: collapse the resolver's continuations to one row per next-token
/// (`Space g` has twelve destinations but is ONE thing to press; a group row
/// names the count instead of listing twelve titles nobody reads at 400 ms),
/// sorted by group then key. `prefix` is what `pendingLabel` prints, as tokens.
export function toView(pending: ChordState, hints: WhichKeyHint[], scope: KbcScope): KeyScopeView {
  const byNext = new Map<string, WhichKeyHint[]>();
  for (const h of hints) {
    const list = byNext.get(h.next);
    if (list) list.push(h);
    else byNext.set(h.next, [h]);
  }
  const rows = [...byNext.entries()].map(([next, group]) => {
    const leaf = group.length === 1 && group[0].rest.length === 1 ? group[0].command : null;
    return {
      keys: [next],
      verb: leaf ? leaf.title : `${group.length} more…`,
      group: leaf ? leaf.group : group[0].command.group,
      isGroup: !leaf,
    };
  });
  rows.sort((a, b) =>
    a.group === b.group ? a.keys[0].localeCompare(b.keys[0]) : a.group.localeCompare(b.group),
  );
  return {
    prefix: [...(pending.count ? [pending.count] : []), ...pending.pending.map(display)],
    rows,
    scope,
  };
}

export default function WhichKey({
  pending,
  hints,
  scope,
}: {
  pending: ChordState;
  hints: WhichKeyHint[];
  scope: KbcScope;
}) {
  return <SharedWhichKey view={toView(pending, hints, scope)} renderKey={display} />;
}
