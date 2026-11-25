// CONVRAIL — what a rail tab needs to know about the terminal it sits beside.
// The registry holds static nodes (no props), so TermShell provides this
// context around both the desktop rail and the phone layer. Pure: no fetching.

import { createContext, useContext } from "react";

export interface TermRailContextValue {
  termId: string;
  harness: string;
  /** The terminal's bound session id; absent until the first harness beat links it. */
  sid?: string;
}

export const TermRailContext = createContext<TermRailContextValue>({ termId: "", harness: "" });

export function useTermRail(): TermRailContextValue {
  return useContext(TermRailContext);
}
