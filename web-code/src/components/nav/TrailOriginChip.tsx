// V70-A6 — the origin chip a trail-linked tab wears (§P7).
//
// "↩ from app/models/order.rb:88 (usage of Order#total, exact)" — the literal
// example from the design. It appears only in a tab that was opened by the
// Ramp's tab/window rungs, which is the only case where the browser's own
// Back is empty and the reader would otherwise have no way home.
//
// THREE HONESTY RULES:
//
//   1. **`u` navigates IN PLACE, as a push.** It never calls
//      `window.opener.focus()`. A `WindowProxy` handle silently no-ops under
//      `noopener`, tab discarding or an OS focus policy, and the research is
//      explicit: never build a feature whose only implementation is that
//      handle. Pushing means the destination the operator just opened stays
//      exactly one Back away — retreat from the retreat is free.
//   2. **No chip rather than a dead chip.** If the trail id in the URL cannot
//      be resolved (a cold open on another device, a tab restored after the
//      origin closed, `BroadcastChannel` unavailable), this renders nothing.
//   3. **The cross-tab count is a live measurement**, taken once on mount by
//      pinging the `kbc-tabs` channel, and absent entirely where the channel
//      is not there. It never claims a number it did not just count.

import { useEffect, useState } from "react";
import { Icon } from "../icons";
import { originChipText, stepAt, type Trail } from "../../lib/trail";
import { countTabsOnTrail } from "../../lib/tabRegistry";

export interface TrailOriginChipProps {
  trail: Trail | null;
  step: number;
  /// Walk back to the origin (a PUSH — see rule 1).
  onReturn(originUrl: string): void;
}

export default function TrailOriginChip({ trail, step, onReturn }: TrailOriginChipProps) {
  const hop = stepAt(trail, step);
  const text = originChipText(hop);
  const [others, setOthers] = useState(0);

  useEffect(() => {
    if (!trail) return;
    let live = true;
    void countTabsOnTrail(trail.id).then((n) => {
      if (live) setOthers(n);
    });
    return () => {
      live = false;
    };
  }, [trail]);

  if (!hop || !text) return null;

  return (
    <span className="kbc-trailchip" data-kbc-trail-chip data-kbc-trail-id={trail?.id}>
      <button
        type="button"
        className="kbc-trailchip__back"
        data-cmd="nav.back"
        data-kbc-trail-return
        title={`${text} — u`}
        onClick={() => onReturn(hop.from)}
      >
        <Icon.ArrowLeft width={12} height={12} aria-hidden />
        <span className="kbc-trailchip__text">{text}</span>
        <kbd className="kbc-trailchip__key">u</kbd>
      </button>
      {others > 0 && (
        <span
          className="kbc-trailchip__tabs"
          data-kbc-trail-tabs={others}
          title="other tabs currently open on this trail"
        >
          +{others} tab{others === 1 ? "" : "s"}
        </span>
      )}
    </span>
  );
}
