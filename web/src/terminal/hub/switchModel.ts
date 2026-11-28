// HUBSWITCH — the switcher's pure grouping and stepping, shared by the hub's
// switcher column and any other surface that lists terminals "This project" /
// "Everywhere". No React, no DOM, no clock: rows in, rows or an id out. The
// input is HUBCORE's `orderHub` output; nothing here re-sorts beyond a stable
// "needs you leads" partition inside each group.

import { needsYouIds, type HubRow } from "./termHub";

export const SWITCH_OPEN_KEY = "kb-term-switch-open";

export interface SwitcherGroups {
  /** Live rows whose project equals the open terminal's project. */
  thisProject: HubRow[];
  /** Every other live row. */
  everywhere: HubRow[];
  /** Ended rows only (never in the other two). */
  ended: HubRow[];
}

/** Stable partition: rows needing the operator first, relative order kept. */
function needsFirst(rows: HubRow[]): HubRow[] {
  return [...rows.filter((r) => r.tier === "needs"), ...rows.filter((r) => r.tier !== "needs")];
}

/**
 * Split ordered hub rows into the switcher's groups. A null `currentProject`
 * (hub page, unknown id) yields no "This project" group. The open terminal
 * stays in its group; the component flags it.
 */
export function groupSwitcher(rows: readonly HubRow[], currentProject: string | null): SwitcherGroups {
  const thisProject: HubRow[] = [];
  const everywhere: HubRow[] = [];
  const ended: HubRow[] = [];
  for (const r of rows) {
    if (r.tier === "ended") ended.push(r);
    else if (currentProject !== null && r.project === currentProject) thisProject.push(r);
    else everywhere.push(r);
  }
  return { thisProject: needsFirst(thisProject), everywhere: needsFirst(everywhere), ended };
}

/** The display order the keys walk: this project, then everywhere; ended excluded. */
export function flattenSwitcher(rows: readonly HubRow[], currentId: string | null): HubRow[] {
  const cur = currentId === null ? undefined : rows.find((r) => r.term.id === currentId);
  const g = groupSwitcher(rows, cur?.project ?? null);
  return [...g.thisProject, ...g.everywhere];
}

function stepIn(ids: readonly string[], currentId: string | null, dir: 1 | -1): string | null {
  if (ids.length === 0) return null;
  const i = currentId === null ? -1 : ids.indexOf(currentId);
  if (i === -1) return dir === 1 ? ids[0] : ids[ids.length - 1];
  if (ids.length === 1) return null;
  return ids[(i + dir + ids.length) % ids.length];
}

/**
 * The id one step from `currentId`, or null when there is nowhere to go.
 * `live` walks the display order (wrapping, ended excluded); `needs` walks the
 * needs-you rows oldest block first (wrapping) and from a row that does not
 * need you jumps to the first of them.
 */
export function stepTerm(
  rows: readonly HubRow[],
  currentId: string | null,
  dir: 1 | -1,
  mode: "live" | "needs",
): string | null {
  if (mode === "live") {
    return stepIn(
      flattenSwitcher(rows, currentId).map((r) => r.term.id),
      currentId,
      dir,
    );
  }
  const ids = needsYouIds(rows);
  if (ids.length === 0) return null;
  if (currentId === null || !ids.includes(currentId)) return ids[0];
  return stepIn(ids, currentId, dir);
}
