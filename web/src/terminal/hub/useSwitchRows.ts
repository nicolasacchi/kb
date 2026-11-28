// HUBSWITCH - the switcher's one data pipeline: the hub list's client join over
// the `["terms"]` rows and the live-status rows, in HUBCORE order. Fetches
// nothing itself beyond those two shared hooks.

import { useMemo } from "react";
import { useLiveSessions } from "../../hooks/useLiveSessions";
import { useTerms } from "../useTerms";
import { joinHub, orderHub, type HubRow } from "./termHub";
import { loadPrefs } from "./termPrefs";

export function useSwitchRows(): HubRow[] {
  const { terms } = useTerms();
  const live = useLiveSessions();
  const prefs = useMemo(() => loadPrefs(), []);
  return useMemo(() => orderHub(joinHub(terms, live.rows, prefs)).rows, [terms, live.rows, prefs]);
}
