// T14 — the ONLY module that dynamically imports `./TerminalPanel`.
// `TerminalSlot` renders `React.lazy(() => import(...this file))`, so
// the panel chunk (which imports the xterm addons and css) is fetched
// only after the user clicks "Open terminal".

import { lazy } from "react";

export interface TerminalPanelProps {
  termId: string;
  sid: string;
  harness: string;
  mode: "watch";
}

const TerminalPanel = lazy(async () => {
  const mod = await import("./TerminalPanel");
  return { default: mod.TerminalPanel };
});

export default TerminalPanel;
