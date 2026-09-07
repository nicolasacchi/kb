// V70-A4 — the Desk's persisted state: a pure, versioned reducer.
//
// The Desk is kb-code v7's five-region shell (docs/research/
// kb-code-v7-continuum-2026-09.html §P1 "The Desk"): left dock · main
// (one or two panes) · right rail · bottom drawer · two icon stripes.
// This module owns the *truth* about that shell's geometry.
//
// ## Why the state lives here and not in `react-resizable-panels`
//
// The library is the resize MECHANISM only. Its `autoSaveId` prop would
// persist an opaque, per-group localStorage blob that presets, the
// keyboard resize submode, `?desk=`, a future CLI and a future URL
// encoding could none of them address — the research report's explicit
// warning (docs/research/kb-code-v7-evidence/research/
// panel-layout-system.md §1.11: "State stays ours. Do **not** use
// `autoSaveId`/`defaultLayout` as the source of truth"). So: sizes flow
// OUT of this reducer into the library (`defaultLayout` at mount, the
// imperative `setLayout` afterwards) and back IN only through
// `onLayoutChanged`'s `isUserInteraction` branch. One serialisable
// object, one home.
//
// ## Purity contract
//
// Nothing in this file reads `Date`, `window`, `localStorage` or any
// other ambient. `loadDeskState`/`saveDeskState` below take an explicit
// `StorageLike` (defaulting to `localStorage` when one exists) so the
// unit suite — which runs `environment: "node"`, see `vitest.config.ts`
// — can exercise every branch including the corrupt-blob ones.

import { DESK_PRESETS, DEFAULT_PRESET, presetState, type DeskPresetName } from "./presets";

/// Bumped only when a shape change cannot be forward-migrated silently.
/// `migrateDeskState` is the ONE place that decides what an older/unknown
/// `v` means; every other consumer may assume the current shape.
export const DESK_STATE_VERSION = 1;

/// The three regions whose size a human can drag. The two stripes are
/// fixed-width (they ARE the collapsed form of dock and rail, and never
/// resize), and `main` has no size of its own — it is whatever the other
/// three leave, which is exactly why the viewport golden is expressed as
/// a floor on main rather than a size for it.
export type DeskRegionId = "dock" | "rail" | "drawer";

/// Everything a separator drag can address. `"panes"` is the split
/// INSIDE main (pane 1's share); it is not a region because it only
/// exists while the URL names a second pane (see `DeskPanesState`).
export type ResizeTarget = DeskRegionId | "panes";

/// The re-cut right rail (§P1: "tab one is All (everything stacked),
/// then Understand …, History …, Review …, Notes"). `"review"` is
/// conditional at RENDER time (only when the open file belongs to an
/// open review) but is always a legal persisted value — a stored
/// `"review"` on a file with no review context renders the honest empty
/// state, never a blank rail and never a silent tab substitution.
/// V72-G1.2 added `dossier`. Like `review` it is a CONDITIONAL tab —
/// `InspectorRail` only offers it while the dossier center is mounted (or
/// while it is the persisted choice, so a stored selection never silently
/// becomes a different tab). It is a real member of the union rather than a
/// mode-local widget because the Desk persists the rail's tab and a tab the
/// reducer could not name would be a tab the desk could not restore.
///
/// V72-J2 added `comments` (D8's comments/1) — UNCONDITIONAL, unlike
/// `review`/`dossier`: every file the scanner covers has an honest
/// (possibly empty) comments/1 list, so there is no "no context for this
/// tab" gate to write.
export type RailTab = "all" | "understand" | "history" | "notes" | "comments" | "review" | "dossier";

export const RAIL_TABS: readonly RailTab[] = [
  "all",
  "understand",
  "history",
  "notes",
  "comments",
  "review",
  "dossier",
];

export interface DeskRegionState {
  /// Percentage of the region's own axis (0..100) — the unit
  /// `react-resizable-panels`' `Layout` map speaks, so no conversion
  /// happens anywhere between this reducer and the library.
  ///
  /// Kept even while `collapsed` is true: collapsing must be reversible
  /// to the size the human last chose, not to the preset default (that
  /// is what `reset` and double-click-a-separator are for).
  size: number;
  collapsed: boolean;
}

export interface DeskPanesState {
  /// How many panes the DESK wants. NOT the source of truth for whether
  /// pane 2 is open — that stays the URL's `?pane2=` (kb-code's Wave-E
  /// ruling, recon/layout-rails-panels.md §8: "pane location is derived
  /// PURELY from the URL"). This field is the preset's INTENT (Explore
  /// asks for two) and what a shell restore aims at; the reader still
  /// renders exactly the panes the URL names, so the two can disagree
  /// without anything drifting.
  count: 1 | 2;
  /// Pane 1's share of the main region (0..100). The only genuinely
  /// load-bearing field here: it is the pane separator's position.
  split: number;
  focused: 1 | 2;
  /// Panes locked against navigation — `placement.ts`'s "file open →
  /// main.focused UNLESS pinned" rule reads this and nothing else.
  pinned: (1 | 2)[];
}

export interface DeskState {
  v: number;
  preset: DeskPresetName;
  regions: Record<DeskRegionId, DeskRegionState>;
  panes: DeskPanesState;
  railTab: RailTab;
  /// Set the moment a human drags a separator or toggles a region
  /// (`isUserInteraction`). A preset is never auto-applied over a dirty
  /// desk — the research report's "sticky manual deviation"
  /// (panel-layout-system.md §3.5): "Auto-layout that overrides a
  /// human's deliberate arrangement is the classic way these systems
  /// earn hatred." An EXPLICIT preset pick (the chip, `?desk=`) always
  /// wins; only implicit application is blocked.
  dirty: boolean;
}

// --- clamps -------------------------------------------------------------
//
// Percentages, not pixels: a stored desk has to survive a window resize
// and a different monitor. The pixel FLOORS that actually protect the
// code column live on the library's `minSize` props in `Desk.tsx` (which
// can express `"160px"`), so these clamps only have to keep the stored
// object sane, never enforce readability on their own.

export const REGION_SIZE_MIN = 6;
export const REGION_SIZE_MAX = 60;
export const SPLIT_MIN = 15;
export const SPLIT_MAX = 85;

export function clampRegionSize(n: number): number {
  if (!Number.isFinite(n)) return REGION_SIZE_MIN;
  return Math.min(REGION_SIZE_MAX, Math.max(REGION_SIZE_MIN, Math.round(n * 100) / 100));
}

export function clampSplit(n: number): number {
  if (!Number.isFinite(n)) return 50;
  return Math.min(SPLIT_MAX, Math.max(SPLIT_MIN, Math.round(n * 100) / 100));
}

// --- actions ------------------------------------------------------------

export type DeskAction =
  /// A separator moved. `user` distinguishes a human drag/keypress from
  /// the library's own non-interactive callbacks (mount, constraint
  /// recompute, a programmatic `setLayout`) — only the former marks the
  /// desk dirty. Mirrors `LayoutChangedMeta.isUserInteraction` exactly.
  | { type: "resize"; target: ResizeTarget; size: number; user?: boolean }
  | { type: "collapse"; region: DeskRegionId; user?: boolean }
  | { type: "expand"; region: DeskRegionId; user?: boolean }
  /// Apply a named preset. `implicit` requests are DROPPED on a dirty
  /// desk (see `DeskState.dirty`); an explicit pick always lands and
  /// clears `dirty`.
  | { type: "setPreset"; preset: DeskPresetName; implicit?: boolean }
  | { type: "setTab"; tab: RailTab }
  | { type: "markDirty" }
  /// Back to the current preset's geometry, `dirty` cleared. The rail
  /// tab is preset-owned too (Review's whole point is landing on the
  /// Review tab), so it resets with everything else.
  | { type: "reset" }
  // --- the three pane actions ------------------------------------------
  // Not in the milestone brief's seven, but `panes.focused`/`pinned`
  // would otherwise be unreachable dead state: something has to write
  // the fields `placement.ts` and the rail's subject read.
  | { type: "focusPane"; pane: 1 | 2 }
  | { type: "setPaneCount"; count: 1 | 2 }
  | { type: "togglePinPane"; pane: 1 | 2 }
  /// V70-A10 ("Workspaces v0") — wholesale-replace the state with an
  /// ALREADY-migrated `DeskState` (the caller is expected to have run it
  /// through `migrateDeskState` first, the SAME "never a bricked shell"
  /// contract `loadDeskState` gives every OTHER reader of a persisted
  /// blob — `useDesk`'s workspace-restore effect is the one caller).
  /// Never marks `dirty` on its own — a restored workspace's geometry IS
  /// its own preset for the rest of this session, not a deviation from
  /// whatever preset happened to be active before the restore.
  | { type: "restore"; state: DeskState };

function withDirty(state: DeskState, user: boolean | undefined): DeskState {
  return user ? { ...state, dirty: true } : state;
}

/// The whole shell's geometry, as one pure function. No IO, no clock, no
/// randomness — `deskState.test.ts` leans on that to golden-pin a
/// round trip over every reachable state.
export function deskReducer(state: DeskState, action: DeskAction): DeskState {
  switch (action.type) {
    case "resize": {
      if (action.target === "panes") {
        const split = clampSplit(action.size);
        if (split === state.panes.split) return withDirty(state, action.user);
        return withDirty({ ...state, panes: { ...state.panes, split } }, action.user);
      }
      const size = clampRegionSize(action.size);
      const cur = state.regions[action.target];
      // A drag that lands ON the collapsed size is a collapse, not a
      // 0-width expanded region — otherwise `collapsed` and `size` tell
      // two different stories and the stripe's pressed state lies.
      const collapsed = size <= REGION_SIZE_MIN ? cur.collapsed : false;
      if (cur.size === size && cur.collapsed === collapsed) return withDirty(state, action.user);
      return withDirty(
        {
          ...state,
          regions: {
            ...state.regions,
            // Keep the last EXPANDED size so expand() can restore it.
            [action.target]: { size: collapsed ? cur.size : size, collapsed },
          },
        },
        action.user,
      );
    }
    case "collapse": {
      if (state.regions[action.region].collapsed) return withDirty(state, action.user);
      return withDirty(
        {
          ...state,
          regions: {
            ...state.regions,
            [action.region]: { ...state.regions[action.region], collapsed: true },
          },
        },
        action.user,
      );
    }
    case "expand": {
      if (!state.regions[action.region].collapsed) return withDirty(state, action.user);
      return withDirty(
        {
          ...state,
          regions: {
            ...state.regions,
            [action.region]: { ...state.regions[action.region], collapsed: false },
          },
        },
        action.user,
      );
    }
    case "setPreset": {
      if (action.implicit && state.dirty) return state;
      const next = presetState(action.preset);
      // The panes' FOCUS and PIN state belong to the operator's current
      // reading position, not to a layout preset — switching Read→Review
      // must not silently move the caret's pane or unlock a pinned one.
      return {
        ...next,
        panes: { ...next.panes, focused: state.panes.focused, pinned: [...state.panes.pinned] },
      };
    }
    case "setTab":
      if (state.railTab === action.tab) return state;
      return { ...state, railTab: action.tab };
    case "markDirty":
      return state.dirty ? state : { ...state, dirty: true };
    case "reset": {
      const next = presetState(state.preset);
      return {
        ...next,
        panes: { ...next.panes, focused: state.panes.focused, pinned: [...state.panes.pinned] },
      };
    }
    case "focusPane":
      if (state.panes.focused === action.pane) return state;
      return { ...state, panes: { ...state.panes, focused: action.pane } };
    case "setPaneCount":
      if (state.panes.count === action.count) return state;
      return { ...state, panes: { ...state.panes, count: action.count } };
    case "togglePinPane": {
      const has = state.panes.pinned.includes(action.pane);
      const pinned = has
        ? state.panes.pinned.filter((p) => p !== action.pane)
        : ([...state.panes.pinned, action.pane].sort() as (1 | 2)[]);
      return { ...state, panes: { ...state.panes, pinned } };
    }
    case "restore":
      return action.state;
    default: {
      // Exhaustiveness: a new action variant fails the build here rather
      // than silently no-op'ing at runtime.
      const never: never = action;
      return never;
    }
  }
}

// --- persistence --------------------------------------------------------

export type StorageLike = Pick<Storage, "getItem" | "setItem" | "removeItem">;

/// Per REPO, not global: a monorepo wants a wider dock than a small
/// crate, and the recon's own open question #4 ("Where would panel widths
/// persist? … widths probably want to be per-repo") is answered here.
export function deskStorageKey(repo: string): string {
  return `kbc:desk:${repo}`;
}

function defaultStorage(): StorageLike | null {
  try {
    return typeof localStorage === "undefined" ? null : localStorage;
  } catch {
    // A privacy mode that throws on the mere property access.
    return null;
  }
}

function isRegion(v: unknown): v is DeskRegionState {
  if (typeof v !== "object" || v === null) return false;
  const r = v as Record<string, unknown>;
  return typeof r.size === "number" && typeof r.collapsed === "boolean";
}

/// Forward-migrate an arbitrary parsed blob into today's shape.
///
/// Returns `null` for anything it cannot honestly read — the caller then
/// falls back to the Read preset. The rule the brief fixes: "on ANY parse
/// failure falls back to the Read preset — never a bricked shell." So
/// this function never throws and never half-reads: a blob is either
/// migrated field-by-field with per-field fallbacks, or rejected whole.
export function migrateDeskState(raw: unknown): DeskState | null {
  if (typeof raw !== "object" || raw === null) return null;
  const o = raw as Record<string, unknown>;
  const v = typeof o.v === "number" ? o.v : null;
  // A FUTURE version (a newer kb-code wrote it, then the operator rolled
  // back) is refused rather than misread: we cannot know what its fields
  // mean. Fail to the preset, loudly-by-behaviour, never a chimera.
  if (v === null || v > DESK_STATE_VERSION) return null;

  const preset: DeskPresetName =
    typeof o.preset === "string" && o.preset in DESK_PRESETS
      ? (o.preset as DeskPresetName)
      : DEFAULT_PRESET;
  const base = presetState(preset);

  const regionsRaw = (o.regions ?? {}) as Record<string, unknown>;
  const regions = { ...base.regions };
  for (const id of ["dock", "rail", "drawer"] as const) {
    const r = regionsRaw[id];
    if (isRegion(r)) regions[id] = { size: clampRegionSize(r.size), collapsed: r.collapsed };
  }

  const panesRaw = (o.panes ?? {}) as Record<string, unknown>;
  const panes: DeskPanesState = {
    count: panesRaw.count === 2 ? 2 : panesRaw.count === 1 ? 1 : base.panes.count,
    split: typeof panesRaw.split === "number" ? clampSplit(panesRaw.split) : base.panes.split,
    focused: panesRaw.focused === 2 ? 2 : 1,
    pinned: Array.isArray(panesRaw.pinned)
      ? (panesRaw.pinned.filter((p) => p === 1 || p === 2) as (1 | 2)[])
      : [],
  };

  const railTab: RailTab =
    typeof o.railTab === "string" && (RAIL_TABS as readonly string[]).includes(o.railTab)
      ? (o.railTab as RailTab)
      : base.railTab;

  return {
    v: DESK_STATE_VERSION,
    preset,
    regions,
    panes,
    railTab,
    dirty: o.dirty === true,
  };
}

/// Read the persisted desk for `repo`, SYNCHRONOUSLY (the caller uses it
/// as a `useReducer` lazy initialiser, so the first paint already has the
/// right geometry — the library's documented percentage-flash caveat).
/// Any failure — no storage, unparseable JSON, a shape this build can't
/// read — lands on the Read preset.
export function loadDeskState(repo: string, storage?: StorageLike | null): DeskState {
  const store = storage === undefined ? defaultStorage() : storage;
  if (!store) return presetState(DEFAULT_PRESET);
  let raw: string | null = null;
  try {
    raw = store.getItem(deskStorageKey(repo));
  } catch {
    return presetState(DEFAULT_PRESET);
  }
  if (raw === null) return presetState(DEFAULT_PRESET);
  let parsed: unknown;
  try {
    parsed = JSON.parse(raw);
  } catch {
    return presetState(DEFAULT_PRESET);
  }
  return migrateDeskState(parsed) ?? presetState(DEFAULT_PRESET);
}

/// V70-A10 ("Workspaces v0") — parse + forward-migrate an arbitrary
/// `desk_json` string (a workspace's saved snapshot, fetched from the
/// server) into today's `DeskState` shape, or `null` on ANY parse failure
/// — the SAME "never a bricked shell" contract [`loadDeskState`] gives a
/// storage read, applied here to a server-fetched string instead of
/// `localStorage`. The caller (`useDesk`'s workspace-restore effect) falls
/// back to leaving the current desk untouched on `null`, exactly like a
/// corrupt persisted blob would fall back to the preset.
export function parseDeskJson(raw: string): DeskState | null {
  let parsed: unknown;
  try {
    parsed = JSON.parse(raw);
  } catch {
    return null;
  }
  return migrateDeskState(parsed);
}

export function saveDeskState(repo: string, state: DeskState, storage?: StorageLike | null): void {
  const store = storage === undefined ? defaultStorage() : storage;
  if (!store) return;
  try {
    store.setItem(deskStorageKey(repo), JSON.stringify(state));
  } catch {
    // Quota/denied — this session still has the state in memory; a
    // reload loses it. Same posture as `useInspectorTab`'s own write.
  }
}
