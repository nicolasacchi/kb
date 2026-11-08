// A4 — the terminal seam: the thin props contract where the kb-term
// `TerminalPanel` mounts later (React.lazy, after an explicit click).
// This unit renders NOTHING — the card's terminal lane is the constant
// `available:false` until the kb-term track fills it. No xterm, no
// WebSocket, no ticket, no `/api/terms` call lives here; T13/T14 own those.

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

export function TerminalSlot(_props: TerminalSlotProps): JSX.Element | null {
  // the kb-term TerminalPanel mounts here (React.lazy, explicit click)
  return null;
}
