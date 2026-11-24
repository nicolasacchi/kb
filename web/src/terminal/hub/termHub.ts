// HUBCORE — everything the Terminals hub decides, as pure functions.
//
// No React, no DOM, no clock: the join of terminal rows with live-session rows,
// the row order, the filters and the one URL builder/parser for `/terminals`.
// Labels and pins arrive as plain arguments (see termPrefs.ts for the browser
// storage). Nothing here fetches anything.

import type { TermRow } from "../../api/terms";
import type { LiveStatusRow } from "../../api/generated/LiveStatusRow";
import { isLaunchHarness } from "../../lib/termLaunch";

export type HubTier = "needs" | "pinned" | "working" | "idle" | "ended";

export interface HubRow {
  term: TermRow;
  /** The live-status row whose `session_id` equals the term's; null when unbound or absent. */
  live: LiveStatusRow | null;
  tier: HubTier;
  label: string | null;
  pinned: boolean;
  /** label, else the live title, else "starting…" (unbound), else the project name. */
  title: string;
  /** Basename of the term's cwd ("/" for the root or an empty cwd). */
  project: string;
  /** Unix seconds the block began (blocked rows only), else null. */
  needsSince: number | null;
}

export interface HubFilter {
  project: string | null;
  harness: string | null;
  needs: boolean;
}

export interface HubUrl {
  /** `/terminals/:id` */
  id: string | null;
  filter: HubFilter;
  /** `?launch=1` */
  launch: boolean;
  /** `?resume=<sid>`; only ever set together with `launch`. */
  resume: string | null;
}

export interface HubUrlInput {
  id?: string | null;
  filter?: Partial<HubFilter>;
  launch?: boolean;
  resume?: string | null;
}

export interface HubPrefs {
  labels: Readonly<Record<string, string>>;
  pins: ReadonlySet<string>;
}

export const HUB_PATH = "/terminals";

// Same grammar as `openTermParam` in components/TermList.tsx (a test pins the
// two against each other); kept local so this module imports no component.
const TERM_ID_RE = /^[a-z0-9][a-z0-9-]{0,31}$/;
const RESUME_RE = /^[A-Za-z0-9][A-Za-z0-9._-]{0,127}$/;
const PROJECT_MAX = 256;
// eslint-disable-next-line no-control-regex
const CONTROL_RE = /[\u0000-\u001f\u007f-\u009f]/;

const TIER_RANK: Readonly<Record<HubTier, number>> = {
  needs: 0,
  pinned: 1,
  working: 2,
  idle: 3,
  ended: 4,
};

function cwdProject(cwd: string): string {
  const parts = cwd.split("/").filter((p) => p !== "");
  return parts.length === 0 ? "/" : parts[parts.length - 1];
}

function isBlocked(term: TermRow, live: LiveStatusRow | null): boolean {
  // The term row and the live row poll at different cadences: either flag counts.
  return term.blocked === true || live?.blocked === true;
}

function tierOf(term: TermRow, live: LiveStatusRow | null, pinned: boolean): HubTier {
  if (term.pane_dead) return "ended";
  if (isBlocked(term, live)) return "needs";
  if (pinned) return "pinned";
  const s = live?.state ?? term.state ?? null;
  if (s === "working" || s === "stalled" || term.state === "working" || term.state === "stalled") {
    return "working";
  }
  return "idle";
}

/** Pure left join: one HubRow per term, input order; first live match wins. */
export function joinHub(
  terms: readonly TermRow[],
  live: readonly LiveStatusRow[],
  prefs: HubPrefs,
): HubRow[] {
  const bySid = new Map<string, LiveStatusRow>();
  for (const l of live) if (!bySid.has(l.session_id)) bySid.set(l.session_id, l);
  return terms.map((term) => {
    const sid = term.session_id ?? null;
    const lv = sid !== null ? (bySid.get(sid) ?? null) : null;
    const rawLabel = Object.prototype.hasOwnProperty.call(prefs.labels, term.id)
      ? prefs.labels[term.id]
      : null;
    const label = rawLabel !== null && rawLabel !== "" ? rawLabel : null;
    const pinned = prefs.pins.has(term.id);
    const project = cwdProject(term.cwd);
    const tier = tierOf(term, lv, pinned);
    const title = label ?? lv?.title ?? (sid === null ? "starting…" : project);
    const needsSince =
      tier === "needs"
        ? ((lv?.blocked ? (lv.blocked_since_unix ?? lv.since_unix) : null) ?? term.started_unix)
        : null;
    return { term, live: lv, tier, label, pinned, title, project, needsSince };
  });
}

function cmpStr(a: string, b: string): number {
  return a < b ? -1 : a > b ? 1 : 0;
}

function cmpRows(a: HubRow, b: HubRow): number {
  const t = TIER_RANK[a.tier] - TIER_RANK[b.tier];
  if (t !== 0) return t;
  let c = 0;
  if (a.tier === "needs") {
    c = (a.needsSince ?? 0) - (b.needsSince ?? 0); // oldest block first
  } else if (a.tier === "pinned") {
    c = cmpStr(a.label ?? "", b.label ?? "");
  } else {
    c = b.term.started_unix - a.term.started_unix; // newest first
  }
  if (c !== 0) return c;
  return cmpStr(a.term.id, b.term.id);
}

/** Total, clock-free order. `rows[0..endedStart)` are live, the rest the ended group. */
export function orderHub(rows: readonly HubRow[]): { rows: HubRow[]; endedStart: number } {
  const out = [...rows].sort(cmpRows);
  const i = out.findIndex((r) => r.tier === "ended");
  return { rows: out, endedStart: i === -1 ? out.length : i };
}

export function filterHub(rows: readonly HubRow[], f: HubFilter): HubRow[] {
  return rows.filter(
    (r) =>
      (f.project === null || r.project === f.project) &&
      (f.harness === null || r.term.harness === f.harness) &&
      (!f.needs || r.tier === "needs"),
  );
}

/**
 * Chips for the filter bar. `harnesses` lists real row values; the URL grammar only
 * carries LAUNCH_HARNESSES (and a bounded, control-free project), so a consumer must
 * offer only URL-representable chips or keep the other filters in component state.
 */
export function hubFilterOptions(rows: readonly HubRow[]): {
  projects: string[];
  harnesses: string[];
} {
  const uniq = (xs: string[]): string[] => [...new Set(xs.filter((x) => x !== ""))].sort(cmpStr);
  return {
    projects: uniq(rows.map((r) => r.project)),
    harnesses: uniq(rows.map((r) => r.term.harness)),
  };
}

/** Ids of the rows that need the operator, oldest block first. */
export function needsYouIds(rows: readonly HubRow[]): string[] {
  return rows
    .filter((r) => r.tier === "needs")
    .sort(cmpRows)
    .map((r) => r.term.id);
}

function validProject(v: unknown): string | null {
  // A project is a cwd basename: it never contains "/" (except the root's own "/").
  return typeof v === "string" &&
    v !== "" &&
    v.length <= PROJECT_MAX &&
    !CONTROL_RE.test(v) &&
    (v === "/" || !v.includes("/"))
    ? v
    : null;
}

function validId(v: unknown): string | null {
  return typeof v === "string" && TERM_ID_RE.test(v) ? v : null;
}

/** The canonical form of any input: invalid parts dropped, resume only with launch. */
function normalise(u: HubUrlInput): HubUrl {
  const launch = u.launch === true;
  const resume = launch && typeof u.resume === "string" && RESUME_RE.test(u.resume) ? u.resume : null;
  return {
    id: validId(u.id),
    filter: {
      project: validProject(u.filter?.project),
      harness: isLaunchHarness(u.filter?.harness as string | null | undefined)
        ? (u.filter?.harness as string)
        : null,
      needs: u.filter?.needs === true,
    },
    launch,
    resume,
  };
}

/** `/terminals`, `/terminals/<id>`, plus `?project=&harness=&needs=1&launch=1&resume=` in that order. */
export function buildHubUrl(input: HubUrlInput = {}): string {
  const u = normalise(input);
  const q = new URLSearchParams();
  if (u.filter.project !== null) q.set("project", u.filter.project);
  if (u.filter.harness !== null) q.set("harness", u.filter.harness);
  if (u.filter.needs) q.set("needs", "1");
  if (u.launch) q.set("launch", "1");
  if (u.resume !== null) q.set("resume", u.resume);
  const qs = q.toString();
  return (u.id !== null ? `${HUB_PATH}/${u.id}` : HUB_PATH) + (qs === "" ? "" : `?${qs}`);
}

const empty = (id: string | null = null): HubUrl => ({
  id,
  filter: { project: null, harness: null, needs: false },
  launch: false,
  resume: null,
});

/**
 * TOTAL: never throws; anything malformed degrades to the defaults. The result
 * has no cwd and no launch harness: `harness` is only the list filter. A launch
 * takes its cwd and harness from the resumed session's own row, never the URL.
 */
export function parseHubUrl(pathname: string, search: string | URLSearchParams): HubUrl {
  try {
    let id: string | null = null;
    if (typeof pathname === "string") {
      const m = /^\/terminals(?:\/([^/]*))?\/?$/.exec(pathname);
      if (m && m[1] !== undefined) id = validId(m[1]);
    }
    let q: URLSearchParams;
    if (typeof search === "string") q = new URLSearchParams(search);
    else if (search instanceof URLSearchParams) q = search;
    else return empty(id);
    const g = (k: string): string | null => {
      const v = q.get(k);
      return typeof v === "string" ? v : null;
    };
    return normalise({
      id,
      filter: { project: g("project"), harness: g("harness"), needs: g("needs") === "1" },
      launch: g("launch") === "1",
      resume: g("resume"),
    });
  } catch {
    return empty();
  }
}
