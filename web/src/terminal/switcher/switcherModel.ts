// TERMSWITCH — the header switcher's model, as pure functions. Rows come from
// HUBCORE (`joinHub` + `orderHub`, nothing re-derived here); the attention
// marks come from TERMNOTIFY's store, passed in as a plain argument.

import type { TermRow } from "../../api/terms";
import type { LiveStatusRow } from "../../api/generated/LiveStatusRow";
import { joinHub, orderHub, type HubPrefs, type HubRow } from "../hub/termHub";
import type { TermMarks } from "../notify/attention";

/** `asks`: the terminal needs the operator. `finished`: a turn ended unseen. */
export type SwitcherGroupKey = "project" | "everywhere";

export interface SwitcherRow {
  id: string;
  title: string;
  project: string;
  harness: string;
  tier: HubRow["tier"];
  /** Needs the operator: the attention store says so OR the hub tier is `needs`. */
  asks: boolean;
  /** A turn ended unseen; never together with `asks`, never on the current terminal. */
  finished: boolean;
  current: boolean;
  hub: HubRow;
}

export interface SwitcherGroup {
  key: SwitcherGroupKey;
  label: string;
  rows: SwitcherRow[];
}

export interface SwitcherModel {
  groups: SwitcherGroup[];
  /** Group order, then row order: the order next/prev and the listbox walk. */
  flat: SwitcherRow[];
  /** Live (not ended) terminals, the current one included. */
  count: number;
  /** Tier-`needs` live terminals: the same count as the hub door badge. */
  needs: number;
}

export const GROUP_LABEL: Readonly<Record<SwitcherGroupKey, string>> = {
  project: "This project",
  everywhere: "Everywhere",
};

export function buildSwitcher(
  terms: readonly TermRow[],
  live: readonly LiveStatusRow[],
  prefs: HubPrefs,
  marks: TermMarks,
  currentId: string | null,
): SwitcherModel {
  const joined = joinHub(terms, live, prefs);
  const ordered = orderHub(joined);
  const liveRows = ordered.rows.slice(0, ordered.endedStart);
  const project = joined.find((r) => r.term.id === currentId)?.project ?? null;
  const rows: SwitcherRow[] = liveRows.map((hub) => {
    const current = hub.term.id === currentId;
    const m = Object.prototype.hasOwnProperty.call(marks, hub.term.id) ? marks[hub.term.id] : undefined;
    const asks = m?.asks === true || hub.tier === "needs";
    return {
      id: hub.term.id,
      title: hub.title,
      project: hub.project,
      harness: hub.term.harness,
      tier: hub.tier,
      asks,
      // The terminal being viewed has no unseen turn end by definition.
      finished: !asks && !current && m?.finishedUnseen === true,
      current,
      hub,
    };
  });
  // Order is orderHub's, never a second one (needs rows already lead each group).
  const groups: SwitcherGroup[] = [];
  const mine = project === null ? [] : rows.filter((r) => r.project === project);
  const rest = project === null ? rows : rows.filter((r) => r.project !== project);
  if (mine.length > 0) groups.push({ key: "project", label: GROUP_LABEL.project, rows: mine });
  if (rest.length > 0) groups.push({ key: "everywhere", label: GROUP_LABEL.everywhere, rows: rest });
  return {
    groups,
    flat: groups.flatMap((g) => g.rows),
    count: rows.length,
    needs: rows.filter((r) => r.tier === "needs").length,
  };
}
