// PHIN2 - what the phone view hands the Drive dock's key bar when direct typing
// is on: the SAME ordered WS input path the proxy types through, the shared
// sticky Ctrl latch and the shared typed-line mirror. A React context carries it
// (the dock is built by the panel, which this unit does not touch); null means
// direct typing is off and the bar keeps using the REST routes.

import { createContext, useContext } from "react";
import type { ConfirmFn } from "./driveApi";
import type { DirectLine } from "./directLine";
import type { StickyMods } from "./stickyMods";

export interface DirectInput {
  /** One WS input frame; false when it was not sent (not live). */
  send(data: string): boolean;
  mods: StickyMods;
  /** The terminal is in application cursor-keys mode (arrows are `ESC O x`). */
  appCursor(): boolean;
  harness: string;
  confirm: ConfirmFn | null;
  line: DirectLine;
}

export const DirectInputContext = createContext<DirectInput | null>(null);

export function useDirectInput(): DirectInput | null {
  return useContext(DirectInputContext);
}
