// TERMNOTIFY - the pure core: which terminals other than the one on screen
// just asked for the operator or finished a turn. No React, no DOM, no clock,
// no query layer; the host feeds it snapshots it already joined.

import type { LiveState } from "../../api/generated/LiveState";

export interface AttnRow {
  termId: string;
  /** Bound session id; null for a terminal with no verified binding. */
  sid: string | null;
  state: LiveState | null;
  blocked: boolean;
  /** The pane is dead: ended terminals never notify. */
  ended: boolean;
  /** Scrubbed live title (or fallback) and project basename only. */
  title: string;
  project: string;
}

export type AttnEvent =
  | { kind: "asks" | "finished"; termId: string; title: string; project: string }
  | { kind: "summary"; count: number; asks: number; finished: number };

/** More than this many events in one derive call fold into one summary. */
export const MAX_INDIVIDUAL = 3;

const WORKING = new Set<LiveState>(["working", "stalled"]);
const TURN_END = new Set<LiveState>(["waiting", "finished"]);

function eligible(r: AttnRow, viewedId: string | null): boolean {
  return r.sid !== null && !r.ended && r.termId !== viewedId;
}

/**
 * `prev === null` (first snapshot, or a reset after a resync/error) never
 * emits. A transition is judged per terminal id between the two snapshots, so
 * the same state seen again emits nothing.
 */
export type TermEvent = Extract<AttnEvent, { termId: string }>;

/** The individual, ordered transitions (asks first, then by id), before coalescing. */
export function transitions(
  prev: readonly AttnRow[] | null,
  next: readonly AttnRow[],
  viewedId: string | null,
): TermEvent[] {
  if (prev === null) return [];
  const before = new Map<string, AttnRow>();
  for (const r of prev) if (!before.has(r.termId)) before.set(r.termId, r);
  const seen = new Set<string>();
  const found: TermEvent[] = [];
  for (const r of next) {
    if (seen.has(r.termId)) continue;
    seen.add(r.termId);
    if (!eligible(r, viewedId)) continue;
    const p = before.get(r.termId);
    if (p === undefined || p.ended) continue; // appeared in this snapshot: baseline only
    const base = { termId: r.termId, title: r.title, project: r.project };
    if (r.blocked) {
      if (!p.blocked) found.push({ kind: "asks", ...base });
    } else if (
      p.state !== null &&
      r.state !== null &&
      WORKING.has(p.state) &&
      TURN_END.has(r.state)
    ) {
      found.push({ kind: "finished", ...base });
    }
  }
  found.sort((a, b) =>
    a.kind !== b.kind ? (a.kind === "asks" ? -1 : 1) : a.termId < b.termId ? -1 : a.termId > b.termId ? 1 : 0,
  );
  return found;
}

export function deriveAttention(
  prev: readonly AttnRow[] | null,
  next: readonly AttnRow[],
  viewedId: string | null,
): { events: AttnEvent[] } {
  const found = transitions(prev, next, viewedId);
  if (found.length > MAX_INDIVIDUAL) {
    const asks = found.filter((e) => e.kind === "asks").length;
    return { events: [{ kind: "summary", count: found.length, asks, finished: found.length - asks }] };
  }
  return { events: found };
}
