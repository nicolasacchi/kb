// TERMSWITCH — the header switcher's model, as pure functions. Rows come from
// HUBCORE (`joinHub` + `orderHub`, nothing re-derived here); the attention
// marks come from TERMNOTIFY's store, passed in as a plain argument.

import type { TermRow } from "../../api/terms";
import type { LiveStatusRow } from "../../api/generated/LiveStatusRow";
import { joinHub, orderHub, type HubPrefs, type HubRow } from "../hub/termHub";
import type { TermMarks } from "../notify/attention";

/** `asks`: the terminal needs the operator. `finished`: a turn ended unseen. */
export type SwitcherMark = "asks" | "finished" | null;
export type SwitcherGroupKey = "project" | "everywhere";

export interface SwitcherRow {
  id: string;
  title: string;
  project: string;
  harness: string;
  mark: SwitcherMark;
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
  /** Live terminals OTHER than the current one that need the operator. */
  needs: number;
}

export const GROUP_LABEL: Readonly<Record<SwitcherGroupKey, string>> = {
  project: "This project",
  everywhere: "Everywhere",
};

function markOf(h: HubRow, marks: TermMarks, current: boolean): SwitcherMark {
  const m = Object.prototype.hasOwnProperty.call(marks, h.term.id) ? marks[h.term.id] : undefined;
  if (m?.asks === true || h.tier === "needs") return "asks";
  // The terminal being viewed has no unseen turn end by definition.
  if (m?.finishedUnseen === true && !current) return "finished";
  return null;
}

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
    return {
      id: hub.term.id,
      title: hub.title,
      project: hub.project,
      harness: hub.term.harness,
      mark: markOf(hub, marks, current),
      current,
      hub,
    };
  });
  // Stable partition: rows that need the operator first, orderHub order kept.
  const sorted = [...rows.filter((r) => r.mark === "asks"), ...rows.filter((r) => r.mark !== "asks")];
  const groups: SwitcherGroup[] = [];
  const mine = project === null ? [] : sorted.filter((r) => r.project === project);
  const rest = project === null ? sorted : sorted.filter((r) => r.project !== project);
  if (mine.length > 0) groups.push({ key: "project", label: GROUP_LABEL.project, rows: mine });
  if (rest.length > 0) groups.push({ key: "everywhere", label: GROUP_LABEL.everywhere, rows: rest });
  return {
    groups,
    flat: groups.flatMap((g) => g.rows),
    count: rows.length,
    needs: rows.filter((r) => r.mark === "asks" && !r.current).length,
  };
}
