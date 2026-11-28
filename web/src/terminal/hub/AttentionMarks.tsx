// HUBSWITCH — the two attention marks a terminal row can carry (TERMNOTIFY's
// store, read through useTermAttention): "asks" (it is waiting on a question)
// and "finished" (a turn ended while you were elsewhere). Distinct text AND
// shape, never colour alone. Shared by the switcher column and the hub list.

import type { ReactElement } from "react";
import { useTermAttention } from "../notify/attention";
import "../../styles/hubSwitcher.css";

export function AttentionMarks({ termId }: { termId: string }): ReactElement | null {
  const mark = useTermAttention()[termId];
  if (!mark || (!mark.asks && !mark.finishedUnseen)) return null;
  return (
    <span className="kb-hubmarks" data-testid="attention-marks">
      {mark.asks ? (
        <span className="kb-hubmark kb-hubmark--asks" data-testid="mark-asks">
          <span aria-hidden="true">? </span>asks
        </span>
      ) : null}
      {mark.finishedUnseen ? (
        <span className="kb-hubmark kb-hubmark--finished" data-testid="mark-finished">
          <span aria-hidden="true">+ </span>finished
        </span>
      ) : null}
    </span>
  );
}
