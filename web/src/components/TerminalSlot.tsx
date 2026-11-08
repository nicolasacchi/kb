// A4 — the terminal seam: the thin props contract where the kb-term
// `TerminalPanel` mounts later (React.lazy, after an explicit click).
// This unit renders NOTHING — the card's terminal lane is the constant
// `available:false` until the kb-term track fills it. No xterm, no
// WebSocket, no ticket, no `/api/terms` call lives here; T13/T14 own those.

import type { ReactElement } from "react";
import type { TerminalLane } from "../api/sessions";

export type TerminalSlotProps = {
  sid: string;
  termId?: string;
  harness: string;
  state?: string;
  // Thin pass-through of the card's terminal lane (A3's constant
  // `{available, reason}` — a structural subset of the future filled
  // lane's `term_id`/`admitted` shape).
  lane: TerminalLane & {
    term_id?: string;
    harness?: string;
    state?: string;
    admitted?: boolean;
  };
};

export function TerminalSlot(_props: TerminalSlotProps): ReactElement | null {
  // the kb-term TerminalPanel mounts here (React.lazy, explicit click)
  return null;
}

// The page renders the terminal wrapper `<details>` only when the slot
// can render something: the lane must be available AND admitted. Today
// the slot always returns null (no panel exists yet), and A3's lane has
// no `admitted` field, so this is false for every card — an
// `available:true` lane alone never shows an empty Terminal section.
// T13/T14 flip `admitted` together with mounting the panel.
export function shouldRenderTerminal(
  lane: TerminalSlotProps["lane"],
): boolean {
  return lane.available === true && lane.admitted === true;
}
