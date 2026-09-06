// V70-A5 — the which-key overlay.
//
// The recon's G4: press `g` in the reader and you got the character `g` in a
// status pip; there are seventeen legal continuations and no way to see them
// without leaving and pressing `?`. Press `[` in the review diff and you got
// nothing at all.
//
// This is a PASSIVE OBSERVER (§P2's word). It never listens for a key, never
// dispatches, and holds no state of its own: `CommandRoot` hands it the
// pending chord and the continuations the resolver already computed, so it
// cannot disagree with what the next keystroke will actually do — and it
// works for EVERY prefix automatically, including ones added later, because
// it renders whatever the registry says extends the sequence.
//
// Bottom-anchored and non-modal on purpose: it must not steal focus, must not
// dim the code you are reading, and must be dismissible by simply continuing
// to type.

import { KBC_LEADER, type KbcCommand, type KbcScope } from "./registry.gen";
import { pendingLabel, type ChordState } from "./dispatch";

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

export default function WhichKey({
  pending,
  hints,
  scope,
}: {
  pending: ChordState;
  hints: WhichKeyHint[];
  scope: KbcScope;
}) {
  // Collapse to one row per next-token: `Space g` has twelve destinations but
  // is ONE thing to press. A group row names the count instead of listing
  // twelve titles nobody reads at 400 ms.
  const byNext = new Map<string, WhichKeyHint[]>();
  for (const h of hints) {
    const list = byNext.get(h.next);
    if (list) list.push(h);
    else byNext.set(h.next, [h]);
  }
  const rows = [...byNext.entries()].map(([next, group]) => {
    const leaf = group.length === 1 && group[0].rest.length === 1 ? group[0].command : null;
    return {
      next,
      leaf,
      label: leaf ? leaf.title : `${group.length} more…`,
      group: leaf ? leaf.group : group[0].command.group,
    };
  });
  rows.sort((a, b) => (a.group === b.group ? a.next.localeCompare(b.next) : a.group.localeCompare(b.group)));

  let lastGroup = "";
  return (
    <div className="kbc-whichkey" data-kbc-whichkey role="status" aria-live="polite">
      <div className="kbc-whichkey__head">
        <kbd className="kbc-whichkey__pending">{pendingLabel(pending)}</kbd>
        <span className="kbc-whichkey__scope">{scope}</span>
        <span className="kbc-whichkey__esc">
          <kbd>Esc</kbd> cancel
        </span>
      </div>
      <div className="kbc-whichkey__rows">
        {rows.map((r) => {
          const head = r.group !== lastGroup ? r.group : null;
          lastGroup = r.group;
          return (
            <span key={r.next} className="kbc-whichkey__cell" data-kbc-whichkey-key={r.next}>
              {head && <span className="kbc-whichkey__group">{head}</span>}
              <kbd>{display(r.next)}</kbd>
              <span className={"kbc-whichkey__label" + (r.leaf ? "" : " is-group")}>{r.label}</span>
            </span>
          );
        })}
      </div>
    </div>
  );
}
