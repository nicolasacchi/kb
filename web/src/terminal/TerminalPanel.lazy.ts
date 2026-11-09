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

export { TerminalPanel as default } from "./TerminalPanel";

export interface TerminalPanelProps {
  termId: string;
  sid: string;
  harness: string;
  mode: "watch";
}
