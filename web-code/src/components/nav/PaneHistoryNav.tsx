// V70-A6 — per-pane back/forward arrows (§P7 · research F3).
//
// The recon found the jumplist was reachable only from inside the CM6 buffer
// (G5) and recorded only pane 1 (G6), with no visible affordance anywhere in
// the chrome. pjeby's Pane Relief is the model, and the research calls its
// feature list "the cheapest high-value idea in the whole survey":
//
//   * per-pane history — the arrows act on THIS pane, not the active one;
//   * a COUNT badge, so Back stops being a slot machine;
//   * a hover preview of where you would land (`path:line`, the line's own
//     text, and the typed edge you took to leave it);
//   * mouse buttons 4/5 navigate the pane UNDER THE POINTER (wired by
//     `Reader.tsx`'s pane wrapper, not here).
//
// The registry rows are `pane.back` / `pane.forward`, deliberately with no
// key column of their own: `Ctrl-o`/`Ctrl-i` already walk the focused pane's
// ring, and a second keyboard home for one action is what the registry exists
// to prevent. `data-cmd` is what makes the buttons visible to the rehearsal
// overlay and to `useLearnMode`'s "this control has a key" toast.

import { useEffect, useState } from "react";
import { Icon } from "../icons";
import {
  backCount,
  forwardCount,
  getNavHistoryState,
  peekBack,
  peekForward,
  subscribeNavHistory,
  type NavLocation,
  type PaneId,
} from "../../lib/navHistory";
import { VIA_LABEL } from "../../lib/trail";

function previewText(loc: NavLocation | null): string | undefined {
  if (!loc) return undefined;
  const head = `${loc.path}:${loc.line}`;
  const bits = [head];
  if (loc.snippet) bits.push(loc.snippet);
  if (loc.via) bits.push(`via ${VIA_LABEL[loc.via]}`);
  return bits.join(" · ");
}

export interface PaneHistoryNavProps {
  pane: PaneId;
  onBack(): void;
  onForward(): void;
}

export default function PaneHistoryNav({ pane, onBack, onForward }: PaneHistoryNavProps) {
  // The ring is a module singleton, not React state, and it changes from
  // three places (a jump here, a jump in the other pane, another tab's merge
  // landing through the `storage` listener). One subscription keeps the
  // badges honest without polling.
  const [, bump] = useState(0);
  useEffect(() => subscribeNavHistory(() => bump((n) => n + 1)), []);

  const state = getNavHistoryState(pane);
  const back = backCount(state);
  const fwd = forwardCount(state);
  const backPreview = previewText(peekBack(state));
  const fwdPreview = previewText(peekForward(state));

  return (
    <span className="kbc-panehist" data-kbc-panehist={pane}>
      <button
        type="button"
        className="kbc-panehist__btn"
        data-cmd="pane.back"
        data-kbc-panehist-back
        disabled={back === 0}
        // The title IS the hover preview — one string, so it is available to
        // a screen reader and to a mouse without a second popup surface to
        // dismiss.
        title={backPreview ? `Back — ${backPreview}` : "Back (nothing older in this pane)"}
        aria-label={backPreview ? `back to ${backPreview}` : "back"}
        onClick={onBack}
      >
        <Icon.ArrowLeft width={13} height={13} aria-hidden />
        {back > 0 && <span className="kbc-panehist__count">{back}</span>}
      </button>
      <button
        type="button"
        className="kbc-panehist__btn kbc-panehist__btn--fwd"
        data-cmd="pane.forward"
        data-kbc-panehist-forward
        disabled={fwd === 0}
        title={fwdPreview ? `Forward — ${fwdPreview}` : "Forward (nothing newer in this pane)"}
        aria-label={fwdPreview ? `forward to ${fwdPreview}` : "forward"}
        onClick={onForward}
      >
        <Icon.ArrowLeft width={13} height={13} aria-hidden />
        {fwd > 0 && <span className="kbc-panehist__count">{fwd}</span>}
      </button>
    </span>
  );
}
