// T14 — the ONLY module that references `./TerminalPanel`.
//
// `TerminalSlot` renders `React.lazy(() => import(...this file))`, so
// this module (and, through its re-export, `./TerminalPanel` and every
// `@xterm/*` package it imports) is fetched only after the user clicks
// "Open terminal". The re-export must stay a plain re-export: wrapping
// `./TerminalPanel` in a second `React.lazy` here makes the slot's
// `lazy()` resolve to a lazy OBJECT rather than the component, and
// React rejects that with error 306 ("Lazy element type must resolve
// to a class or function") — the whole panel then never mounts.

import type { ReactNode } from "react";

export { TerminalPanel as default } from "./TerminalPanel";

export interface TerminalPanelProps {
  termId: string;
  sid: string;
  harness: string;
  /** Close unmounts the panel (disconnect only) via the slot owner. */
  onClose?: () => void;
  /** The program rang the bell while nobody was looking (FUTERMATTN). */
  onBell?: () => void;
  /** Phone sheet only: extra controls in the title bar (End, terminal switcher). */
  headerExtra?: ReactNode;
  /**
   * BHEAD: controls rendered immediately left of the pane verbs (the command
   * palette button, the terminal switcher). Desktop header and phone extra row.
   */
  headerActions?: ReactNode;
  /**
   * BHEAD: text pasted ONCE into the terminal when it first reaches live, via the
   * paste path and without Enter (one trailing newline is stripped). Never
   * re-sent on reconnect, remount or re-render with the same text.
   */
  initialInput?: string;
}
