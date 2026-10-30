// OL PRES3 - presence in the file tree, in kb-code's own trust vocabulary.
//
// TRIGGER vs ATTRIBUTION. A row lights when `kbc:mirror.updated` NAMES it -
// the SPA already receives that frame, and it fires whatever changed the
// file (an agent's edit tool, `sed`, `git checkout`, the operator's editor).
// `GET /api/kb/presence` (kbc-presence/1, OL PRES2) never lights anything by
// itself; it only ATTRIBUTES a touch the mirror already reported:
//
//   exact      a beat path equal to the touched path, whose beat time lies
//              within `ATTRIBUTION_WINDOW_SECS` of the mirror event
//   candidate  no such beat, but exactly ONE `working` session maps to the
//              repo's main checkout ("probably session 3f2a")
//   neutral    neither: "changed 20 s ago", no hue, no session
//
// Hue is identity only (`provHue.ts`, rule 2); the rail STYLE carries trust
// (Lane Budget: solid / dotted / plain), so a hue collision between two of
// the 8 hues never reads as a claim of sameness.
//
// Pure functions only: every clock is passed in. Times in this module are
// `performance.now()` milliseconds (monotonic, wall-clock independent): a
// beat's time is `fetchedPerf - age_secs * 1000`, never `Date.now()`, so a
// skewed client clock cannot move a touch in or out of the window.

import { ancestorDirs } from "./tree";

export const PRESENCE_SCHEMA = "kbc-presence/1";

/// A mirror touch and a beat count as the same edit within this window.
export const ATTRIBUTION_WINDOW_SECS = 15;
/// The ring: an unopened touch stays this long, then is forgotten.
export const RING_SECS = 3600;
/// A single `mirror.updated` naming more paths than this is a checkout or
/// rebase (or a mass formatter run), not an edit: it is never marked per path.
/// The tree shows one muted line for `BURST_SECS`; the `repo.head_moved`
/// banner appears separately, and only if HEAD actually moved.
export const BURST_PATHS = 20;
/// The tree's "N files changed at once" line expires after this long.
export const BURST_SECS = 120;
/// A touch is attributed (and then frozen) from the first snapshot fetched at
/// least this long after it: the grace absorbs relay lag between the edit and
/// the beat reaching kb.
export const ATTRIBUTION_GRACE_MS = 3000;
/// The window the Home / Spine summary counts files in.
export const SUMMARY_WINDOW_SECS = 600;

/// kbc-presence/1 (hand-written: kb-code's SPA has no generated binding).
export interface PresenceFileWire {
  repo: string;
  /// Basename of a linked worktree; absent for the main checkout.
  worktree?: string;
  path: string;
  session_id: string;
  harness: string;
  /// kb's live-state names, snake_case: working | stalled | waiting | cold |
  /// finished | presumed_ended.
  state: string;
  age_secs: number;
}

export interface PresenceWire {
  schema: string;
  available: boolean;
  caption?: string;
  now_unix: number;
  files: PresenceFileWire[];
}

/// A fetched answer plus the monotonic instant it arrived.
export interface PresenceSnapshot extends PresenceWire {
  fetchedPerf: number;
}

export type FadeLevel = 0 | 1 | 2 | 3;

/// Age -> brightness (3 brightest). A state that says the session is not
/// actively editing caps how bright the mark may be: `stalled` at 2;
/// `waiting`, `cold`, `finished`, `presumed_ended` at 1. An unknown state is
/// uncapped (the age alone decides).
export function fadeLevel(ageSecs: number, state: string): FadeLevel {
  const age = Math.max(0, ageSecs);
  const byAge: FadeLevel = age < 60 ? 3 : age < 180 ? 2 : age < 600 ? 1 : 0;
  let cap: FadeLevel = 3;
  if (state === "stalled") cap = 2;
  else if (
    state === "waiting" ||
    state === "cold" ||
    state === "finished" ||
    state === "presumed_ended"
  ) {
    cap = 1;
  }
  return Math.min(byAge, cap) as FadeLevel;
}

/// Seconds since `fetchedPerf` plus the age the server stated at fetch time.
export function ageNow(ageSecsAtFetch: number, fetchedPerf: number, nowPerf: number): number {
  return Math.max(0, ageSecsAtFetch) + Math.max(0, nowPerf - fetchedPerf) / 1000;
}

/// One path the mirror named, stamped on arrival (perf ms).
export interface MirrorTouch {
  repo: string;
  path: string;
  at: number;
  /// Set once, by `freezeTouches`, and never changed: attribution is a claim
  /// about the moment of the change, not a live view of today's sessions.
  attr?: AttributedTouch;
}

export type Trust = "exact" | "candidate" | "neutral";

export interface AttributedTouch {
  path: string;
  at: number;
  trust: Trust;
  sessionId?: string;
  harness?: string;
  state?: string;
  /// Other sessions whose beat also matched (exact only).
  others: number;
}

function beatAt(f: PresenceFileWire, snap: PresenceSnapshot): number {
  return snap.fetchedPerf - Math.max(0, f.age_secs) * 1000;
}

/// Attribute each touch. Worktree rows are never evidence about the main
/// checkout's file, so they never attribute (they have their own summary).
export function attribute(
  touches: readonly MirrorTouch[],
  snap: PresenceSnapshot | undefined,
): AttributedTouch[] {
  const main = snap && snap.available ? snap.files.filter((f) => !f.worktree) : [];
  const working = new Map<string, PresenceFileWire>();
  for (const f of main) {
    if (f.state === "working" && !working.has(f.session_id)) working.set(f.session_id, f);
  }
  const window = ATTRIBUTION_WINDOW_SECS * 1000;
  return touches.map((t) => {
    if (t.attr) return t.attr;
    const hits = snap
      ? main
          .filter((f) => f.path === t.path && Math.abs(beatAt(f, snap) - t.at) <= window)
          .sort((a, b) => Math.abs(beatAt(a, snap) - t.at) - Math.abs(beatAt(b, snap) - t.at))
      : [];
    if (hits.length > 0) {
      const best = hits[0];
      const sessions = new Set(hits.map((h) => h.session_id));
      return {
        path: t.path,
        at: t.at,
        trust: "exact" as const,
        sessionId: best.session_id,
        harness: best.harness,
        state: best.state,
        others: sessions.size - 1,
      };
    }
    if (working.size === 1) {
      const only = [...working.values()][0];
      return {
        path: t.path,
        at: t.at,
        trust: "candidate" as const,
        sessionId: only.session_id,
        harness: only.harness,
        state: "working",
        others: 0,
      };
    }
    return { path: t.path, at: t.at, trust: "neutral" as const, others: 0 };
  });
}

export function shortSid(sid: string): string {
  return sid.slice(0, 4);
}

/// "20 s ago" / "25 min ago" / "2 h ago".
export function ageText(secs: number): string {
  const s = Math.max(0, Math.floor(secs));
  if (s < 60) return `${s} s ago`;
  if (s < 3600) return `${Math.floor(s / 60)} min ago`;
  return `${Math.floor(s / 3600)} h ago`;
}

export interface PresenceMark {
  path: string;
  level: 1 | 2 | 3;
  trust: Trust;
  sessionId?: string;
  harness?: string;
  others: number;
  ageSecs: number;
  /// Past the 10-minute fade and still unopened.
  unopened: boolean;
  label: string;
}

function markLabel(a: AttributedTouch, ageSecs: number, unopened: boolean): string {
  const when = ageText(ageSecs);
  let base: string;
  if (a.trust === "exact" && a.sessionId) {
    base = `Edited by ${a.harness || "agent"} session ${shortSid(a.sessionId)} ${when}`;
    if (a.others > 0) base += `, plus ${a.others} other`;
  } else if (a.trust === "candidate" && a.sessionId) {
    base = `Probably ${a.harness || "agent"} session ${shortSid(a.sessionId)}, changed ${when}`;
  } else {
    base = `Changed ${when}`;
  }
  return unopened ? `${base}, not opened since` : base;
}

/// The marks to draw. `openedAt(path)` is the perf instant the operator last
/// opened the file (undefined = never): a touch at or before it is cleared.
/// An unopened touch dims to level 1 and stays until the ring expires.
export function buildMarks(
  attributed: readonly AttributedTouch[],
  openedAt: (path: string) => number | undefined,
  nowPerf: number,
): Map<string, PresenceMark> {
  const out = new Map<string, PresenceMark>();
  for (const a of attributed) {
    const opened = openedAt(a.path);
    if (opened !== undefined && opened >= a.at) continue;
    const ageSecs = Math.max(0, (nowPerf - a.at) / 1000);
    if (ageSecs >= RING_SECS) continue;
    const faded = fadeLevel(ageSecs, a.trust === "neutral" ? "" : a.state ?? "");
    const level = Math.max(1, faded) as 1 | 2 | 3;
    const unopened = ageSecs >= 600;
    out.set(a.path, {
      path: a.path,
      level,
      trust: a.trust,
      sessionId: a.sessionId,
      harness: a.harness,
      others: a.others,
      ageSecs,
      unopened,
      label: markLabel(a, ageSecs, unopened),
    });
  }
  return out;
}

export interface DirRollup {
  /// Freshest descendant's level / trust / session.
  level: 1 | 2 | 3;
  trust: Trust;
  sessionId?: string;
  /// Distinct marked files beneath.
  count: number;
  label: string;
}

/// Ancestor roll-up: a collapsed directory shows its freshest marked
/// descendant's rail plus the distinct file count.
export function rollup(marks: ReadonlyMap<string, PresenceMark>): Map<string, DirRollup> {
  const out = new Map<string, DirRollup>();
  const freshest = new Map<string, PresenceMark>();
  const counts = new Map<string, number>();
  for (const m of marks.values()) {
    for (const dir of ancestorDirs(m.path)) {
      counts.set(dir, (counts.get(dir) ?? 0) + 1);
      const cur = freshest.get(dir);
      if (!cur || m.ageSecs < cur.ageSecs) freshest.set(dir, m);
    }
  }
  for (const [dir, m] of freshest) {
    const count = counts.get(dir) ?? 0;
    out.set(dir, {
      level: m.level,
      trust: m.trust,
      sessionId: m.sessionId,
      count,
      label: `${count} changed file${count === 1 ? "" : "s"} inside; freshest: ${m.label}`,
    });
  }
  return out;
}

export interface WorktreeCount {
  worktree: string;
  files: number;
}

/// Touches made inside linked worktrees: a muted count + caption, never a
/// mark on the main checkout's file of the same relative path.
export function worktreeCounts(
  snap: PresenceSnapshot | undefined,
  nowPerf: number,
): WorktreeCount[] {
  if (!snap || !snap.available) return [];
  const by = new Map<string, Set<string>>();
  for (const f of snap.files) {
    if (!f.worktree) continue;
    if (ageNow(f.age_secs, snap.fetchedPerf, nowPerf) >= SUMMARY_WINDOW_SECS) continue;
    const set = by.get(f.worktree) ?? new Set<string>();
    set.add(f.path);
    by.set(f.worktree, set);
  }
  return [...by.entries()]
    .map(([worktree, s]) => ({ worktree, files: s.size }))
    .sort((a, b) => a.worktree.localeCompare(b.worktree));
}

export interface PresenceSummary {
  working: number;
  files: number;
  text: string;
}

/// "1 session working · 4 files in 10 min", or null when there is nothing to
/// say. Counts every row the relay returned for the repo (main checkout and
/// worktrees): it is a pulse, not an attribution.
export function summarize(
  snap: PresenceSnapshot | undefined,
  nowPerf: number,
): PresenceSummary | null {
  if (!snap || !snap.available) return null;
  const working = new Set<string>();
  const files = new Set<string>();
  for (const f of snap.files) {
    if (f.state === "working") working.add(f.session_id);
    if (ageNow(f.age_secs, snap.fetchedPerf, nowPerf) < SUMMARY_WINDOW_SECS) {
      files.add(`${f.worktree ?? ""}\0${f.path}`);
    }
  }
  const parts: string[] = [];
  if (working.size > 0) {
    parts.push(`${working.size} session${working.size === 1 ? "" : "s"} working`);
  }
  if (files.size > 0) parts.push(`${files.size} file${files.size === 1 ? "" : "s"} in 10 min`);
  if (parts.length === 0) return null;
  return { working: working.size, files: files.size, text: parts.join(" · ") };
}
