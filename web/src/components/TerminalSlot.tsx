// A4 — the terminal seam: the thin props contract where the kb-term
// `TerminalPanel` mounts later (React.lazy, after an explicit click).
// This unit renders NOTHING — the panel does not exist yet, so the slot
// stays empty even when T13's registry-derived lane is available and
// admitted. No xterm, no WebSocket, no ticket, no `/api/terms` call
// lives here; T14 owns those.

import type { ReactElement } from "react";
import type { TerminalLane } from "../api/sessions";

export type TerminalSlotProps = {
  sid: string;
  termId?: string;
  harness: string;
  state?: string;
  // The card's T13 terminal lane (available, optional reason/term_id/
  // harness/state/blocked/via, always-present admitted).
  lane: TerminalLane,
};

export function TerminalSlot(_props: TerminalSlotProps): ReactElement | null {
  // the kb-term TerminalPanel mounts here (React.lazy, explicit click)
  return null;
}

// FXA4 — `shouldRenderTerminal` is the page's gate for the terminal
// `<details>` wrapper. The slot renders NOTHING today (no panel exists
// — T14 mounts it), so the gate MUST stay false: rendering the wrapper
// whenever the lane is available+admitted would show an empty 'Terminal'
// section. T14 flips this to `lane.available && lane.admitted` when the
// panel actually renders. (Brief A4 §3.1: the wrapper renders only if
// the slot renders something, which today is never.)
export function shouldRenderTerminal(
  lane: TerminalSlotProps["lane"],
): boolean {
  void lane;
  return false;
}
