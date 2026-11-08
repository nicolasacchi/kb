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

// The page renders the terminal wrapper `<details>` only when the slot
// can render something: the lane must be available AND admitted. The
// slot always returns null (no panel exists yet — T14 mounts it), so an
// `available:true` lane alone never shows an empty Terminal section.
export function shouldRenderTerminal(
  lane: TerminalSlotProps["lane"],
): boolean {
  return lane.available === true && lane.admitted === true;
}
