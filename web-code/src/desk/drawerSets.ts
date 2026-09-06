// V70-A4 — the bottom drawer's result-set ring.
//
// docs/research/kb-code-v7-continuum-2026-09.html §P1: "The bottom drawer
// is a tab bar of named, stable-id result sets … a hard cap with visible
// eviction (an evicted tab greys with 'closed — Space u to reopen');
// pinned sets never evict; **eviction is a view operation, never a data
// operation**."
//
// That last clause is the whole design of this module. When the ring
// overflows, the losing set is MARKED `evicted` and keeps every one of
// its rows — reopening it is a state flip, never a re-query. A set only
// stops existing when a human closes it (`drop`) or when the retention
// ceiling below releases the oldest already-evicted one, which is a
// memory bound and is captioned as such, never dressed up as eviction.
//
// In-memory ONLY (no localStorage, no server). A result set is the
// product of a query the operator just ran; resurrecting yesterday's
// `gr` on today's working tree would be a stale answer wearing a fresh
// tab. Kept deliberately, not overlooked.
//
// Pure: no clock, no storage, no ids from `Math.random`. `seq` is a
// monotonic counter carried IN the state, so the same action sequence
// always produces the same object — which is what lets the unit suite
// golden-pin the eviction order.

export const DRAWER_SET_CAP = 9;

/// How many EVICTED sets keep their rows before the oldest is released.
/// Not part of the cap contract above: this is a memory ceiling on a
/// browser tab that could otherwise accumulate every result set of a
/// long session. Reaching it drops the row DATA, so the tab disappears
/// rather than pretending "reopen" would still work.
export const DRAWER_EVICTED_RETAIN = 9;

export type DrawerSetKind = "usages" | "definitions" | "search" | "diagnostics" | "recipe" | "findings";

export interface DrawerRow {
  repo: string;
  path: string;
  line: number;
  /// The excerpt line, when the producing query had one.
  text?: string;
  /// `exact` / `likely` / `candidate` — rendered as a section, never
  /// mixed into a single ranked list (§P1: "ordered exact ▷ likely ▷
  /// candidate ▷ observed").
  trustClass?: string;
  /// The producing query's own honesty flag, carried per row so the
  /// drawer never has to re-derive it.
  approximate?: boolean;
}

export interface DrawerSet {
  /// V71-E2 — what the TAB BADGE shows, when it is not `rows.length`. A
  /// set whose owner renders its own body (the Usages dock) keeps `rows`
  /// empty, and a tab reading `0` beside a census reading `1,841` is
  /// exactly the disagreeing-counts failure this milestone exists to stop.
  /// `undefined` ⇒ `rows.length`, byte-identical to every pre-E2 set.
  count?: number;
  /// Stable across re-runs of the same query — `usages:word@repo/path:line`.
  /// Re-keeping the same query REPLACES the set's rows in place instead
  /// of opening a second identical tab.
  id: string;
  title: string;
  kind: DrawerSetKind;
  rows: DrawerRow[];
  pinned: boolean;
  evicted: boolean;
  /// Row cursor, for `j`/`k`/`Enter` walking.
  cursor: number;
  /// Insertion order. Monotonic; the eviction victim is the smallest
  /// `seq` among unpinned, non-active, live sets.
  seq: number;
}

export interface DrawerSetsState {
  sets: DrawerSet[];
  activeId: string | null;
  /// Next `seq` to hand out. In the state (not a module-level counter)
  /// so the reducer stays a pure function of its inputs.
  nextSeq: number;
}

export const initialDrawerSets: DrawerSetsState = { sets: [], activeId: null, nextSeq: 1 };

export interface DrawerSetInput {
  id: string;
  title: string;
  kind: DrawerSetKind;
  rows: DrawerRow[];
  /// See [`DrawerSet.count`].
  count?: number;
}

/// What a tab's badge reads. One function, so the tab bar and any test
/// agree on the rule rather than each re-deriving it.
export function drawerSetCount(set: DrawerSet): number {
  return set.count ?? set.rows.length;
}

export type DrawerSetsAction =
  /// Keep a live result set in the drawer. Same `id` ⇒ refresh in place
  /// (rows replaced, cursor clamped, pin/seq preserved, un-evicted).
  | { type: "keep"; set: DrawerSetInput }
  | { type: "activate"; id: string }
  /// Close a tab = evict it (a VIEW operation; rows survive).
  | { type: "close"; id: string }
  | { type: "reopen"; id: string }
  /// Really remove it, rows and all — the only data operation here.
  | { type: "drop"; id: string }
  | { type: "togglePin"; id: string }
  /// Move the active set's row cursor.
  | { type: "moveCursor"; delta: number }
  | { type: "setCursor"; index: number }
  /// Step between LIVE tabs (`]d` / `[d`).
  | { type: "stepSet"; delta: number };

/// `usages:KNOWN_SYMBOL@fixture/src/lib.rs:12` — the stable id §P1 asks
/// for ("Each set has a stable id … which is what makes CLI parity
/// possible"). Deterministic from the query, so the same `gr` twice is
/// one tab, refreshed.
export function drawerSetId(kind: DrawerSetKind, key: string): string {
  return `${kind}:${key}`;
}

function liveSets(state: DrawerSetsState): DrawerSet[] {
  return state.sets.filter((s) => !s.evicted);
}

/// The eviction victim: the oldest LIVE set that is neither pinned nor
/// active. Returns `null` when every live set is protected — in which
/// case the cap simply does not bite (a desk full of pinned sets is a
/// deliberate arrangement, and silently evicting one would be exactly
/// the "auto-layout overrides the human" failure the design forbids).
function evictionVictim(sets: DrawerSet[], activeId: string | null): DrawerSet | null {
  let victim: DrawerSet | null = null;
  for (const s of sets) {
    if (s.evicted || s.pinned || s.id === activeId) continue;
    if (victim === null || s.seq < victim.seq) victim = s;
  }
  return victim;
}

/// Release the oldest evicted sets past the retention ceiling. Returns a
/// new array; never touches a live or pinned set.
function applyRetention(sets: DrawerSet[]): DrawerSet[] {
  const evicted = sets.filter((s) => s.evicted && !s.pinned);
  if (evicted.length <= DRAWER_EVICTED_RETAIN) return sets;
  const doomed = new Set(
    [...evicted].sort((a, b) => a.seq - b.seq).slice(0, evicted.length - DRAWER_EVICTED_RETAIN).map((s) => s.id),
  );
  return sets.filter((s) => !doomed.has(s.id));
}

function clampCursor(rows: number, cursor: number): number {
  if (rows === 0) return 0;
  return Math.min(rows - 1, Math.max(0, cursor));
}

export function drawerSetsReducer(state: DrawerSetsState, action: DrawerSetsAction): DrawerSetsState {
  switch (action.type) {
    case "keep": {
      const existing = state.sets.find((s) => s.id === action.set.id);
      if (existing) {
        const sets = state.sets.map((s) =>
          s.id === action.set.id
            ? {
                ...s,
                title: action.set.title,
                kind: action.set.kind,
                rows: action.set.rows,
                count: action.set.count,
                evicted: false,
                cursor: clampCursor(action.set.rows.length, s.cursor),
              }
            : s,
        );
        return { ...state, sets, activeId: action.set.id };
      }
      const fresh: DrawerSet = {
        id: action.set.id,
        title: action.set.title,
        kind: action.set.kind,
        rows: action.set.rows,
        count: action.set.count,
        pinned: false,
        evicted: false,
        cursor: 0,
        seq: state.nextSeq,
      };
      let sets = [...state.sets, fresh];
      // Cap the LIVE set count. The newcomer is the active one, so the
      // victim search below never picks it.
      while (liveSets({ ...state, sets, activeId: fresh.id }).length > DRAWER_SET_CAP) {
        const victim = evictionVictim(sets, fresh.id);
        if (!victim) break;
        sets = sets.map((s) => (s.id === victim.id ? { ...s, evicted: true } : s));
      }
      return { sets: applyRetention(sets), activeId: fresh.id, nextSeq: state.nextSeq + 1 };
    }
    case "activate": {
      const target = state.sets.find((s) => s.id === action.id);
      if (!target) return state;
      // Activating an evicted tab is exactly "reopen" — one gesture, not
      // two (the greyed tab is clickable and says "closed — reopen").
      if (target.evicted) return drawerSetsReducer(state, { type: "reopen", id: action.id });
      if (state.activeId === action.id) return state;
      return { ...state, activeId: action.id };
    }
    case "close": {
      const target = state.sets.find((s) => s.id === action.id);
      if (!target || target.evicted) return state;
      const sets = applyRetention(state.sets.map((s) => (s.id === action.id ? { ...s, evicted: true } : s)));
      const stillActive = state.activeId === action.id;
      const nextActive = stillActive ? (liveSets({ ...state, sets })[0]?.id ?? null) : state.activeId;
      return { ...state, sets, activeId: nextActive };
    }
    case "reopen": {
      const target = state.sets.find((s) => s.id === action.id);
      if (!target || !target.evicted) return state;
      let sets = state.sets.map((s) => (s.id === action.id ? { ...s, evicted: false } : s));
      while (liveSets({ ...state, sets, activeId: action.id }).length > DRAWER_SET_CAP) {
        const victim = evictionVictim(sets, action.id);
        if (!victim) break;
        sets = sets.map((s) => (s.id === victim.id ? { ...s, evicted: true } : s));
      }
      return { ...state, sets: applyRetention(sets), activeId: action.id };
    }
    case "drop": {
      const sets = state.sets.filter((s) => s.id !== action.id);
      if (sets.length === state.sets.length) return state;
      const nextActive =
        state.activeId === action.id ? (sets.filter((s) => !s.evicted)[0]?.id ?? null) : state.activeId;
      return { ...state, sets, activeId: nextActive };
    }
    case "togglePin": {
      const target = state.sets.find((s) => s.id === action.id);
      if (!target) return state;
      return { ...state, sets: state.sets.map((s) => (s.id === action.id ? { ...s, pinned: !s.pinned } : s)) };
    }
    case "moveCursor":
    case "setCursor": {
      if (!state.activeId) return state;
      const active = state.sets.find((s) => s.id === state.activeId);
      if (!active || active.rows.length === 0) return state;
      const next =
        action.type === "moveCursor"
          ? clampCursor(active.rows.length, active.cursor + action.delta)
          : clampCursor(active.rows.length, action.index);
      if (next === active.cursor) return state;
      return { ...state, sets: state.sets.map((s) => (s.id === active.id ? { ...s, cursor: next } : s)) };
    }
    case "stepSet": {
      const live = liveSets(state);
      if (live.length === 0) return state;
      const at = live.findIndex((s) => s.id === state.activeId);
      // Wraps, like `[f`/`]f` already do for the working set.
      const idx = at === -1 ? 0 : (((at + action.delta) % live.length) + live.length) % live.length;
      return { ...state, activeId: live[idx].id };
    }
    default: {
      const never: never = action;
      return never;
    }
  }
}

/// The tab bar's own order: live sets first in insertion order, then the
/// evicted ones (greyed, still clickable). Evicted tabs never reshuffle
/// the live ones — the whole point of visible eviction is that the row
/// you were reading does not move under you.
export function drawerTabOrder(state: DrawerSetsState): DrawerSet[] {
  const live = state.sets.filter((s) => !s.evicted).sort((a, b) => a.seq - b.seq);
  const gone = state.sets.filter((s) => s.evicted).sort((a, b) => a.seq - b.seq);
  return [...live, ...gone];
}

export function activeDrawerSet(state: DrawerSetsState): DrawerSet | null {
  if (!state.activeId) return null;
  return state.sets.find((s) => s.id === state.activeId) ?? null;
}
