// V70-A4 — `useDesk`: the one owner of the shell's state.
//
// Three kinds of state live here, deliberately separated:
//
//  1. **Persisted geometry** (`DeskState`) — sizes, collapse, preset,
//     rail tab, dirty. Per repo, in localStorage, read synchronously as
//     the reducer's lazy initialiser so the first paint is already
//     right.
//  2. **Transient view state** — `chrome` (full/focus/present), `zoom`,
//     the resize submode, the region with focus. NONE of it persists: a
//     reload must never land the operator in a zoomed, chrome-less desk
//     they have to guess their way out of. Focus and Present are modes
//     you ENTER, so they are modes you leave by reloading too.
//  3. **The drawer's result-set ring** (`drawerSets.ts`) — in-memory
//     only, for the reason that module documents.
//
// Everything the shell renders is derived from those three by pure
// functions, so there is no fourth place a size can hide.

import { useCallback, useEffect, useMemo, useReducer, useRef, useState } from "react";
import {
  deskReducer,
  loadDeskState,
  saveDeskState,
  type DeskAction,
  type DeskRegionId,
  type DeskState,
  type RailTab,
  type ResizeTarget,
} from "./deskState";
import { DESK_PRESETS, presetState, type DeskPresetName } from "./presets";
import {
  drawerSetsReducer,
  initialDrawerSets,
  type DrawerSetInput,
  type DrawerSetsAction,
  type DrawerSetsState,
} from "./drawerSets";
import type { DeskFocusRegion } from "./resizeSubmode";
import { useDeskOverride } from "../lib/deskParam";

/// `full` is the ordinary desk. `focus` (F11) collapses the docks and
/// the drawer but KEEPS the stripes and adds a status line — JetBrains'
/// two-tier treatment, and NN/g's warning against a single binary zen
/// toggle (panel-layout-system.md §3.5). `present` (Shift-F11) also
/// hides the stripes and enlarges the type, leaving one hint line.
export type DeskChrome = "full" | "focus" | "present";

export interface DeskApi {
  state: DeskState;
  dispatch: (a: DeskAction) => void;

  // --- transient view state ---------------------------------------------
  chrome: DeskChrome;
  setChrome: (c: DeskChrome) => void;
  /// The region zoomed to fill the shell (`Ctrl-w m`), or `null`.
  zoom: DeskFocusRegion | null;
  toggleZoom: (region?: DeskFocusRegion) => void;
  focusRegion: DeskFocusRegion;
  setFocusRegion: (r: DeskFocusRegion) => void;
  resizeMode: boolean;
  setResizeMode: (on: boolean) => void;

  // --- convenience wrappers the shell + the reader both use --------------
  toggleRegion: (region: DeskRegionId) => void;
  applyPreset: (preset: DeskPresetName, opts?: { implicit?: boolean }) => void;
  setRailTab: (tab: RailTab) => void;
  nudge: (target: ResizeTarget, delta: number) => void;
  equalise: () => void;

  // --- the drawer's result-set ring --------------------------------------
  drawer: DrawerSetsState;
  drawerDispatch: (a: DrawerSetsAction) => void;
  keepInDrawer: (set: DrawerSetInput) => void;

  /// `true` once the operator has deviated from the preset — the chip
  /// renders "Read ·" vs "Read · edited".
  dirty: boolean;
  fontScale: number;

  /// V70-A10 ("Workspaces v0") — wholesale-replace the persisted geometry
  /// with an already-migrated `DeskState` (a workspace's saved snapshot,
  /// `deskState.ts`'s `parseDeskJson`). Thin `dispatch({type:"restore"})`
  /// wrapper — kept as its own method so a caller (`Reader.tsx`'s
  /// workspace-open effect) never has to import `DeskAction` just for
  /// this one call.
  restore: (state: DeskState) => void;
}

/// The size a region should CURRENTLY render at, given the persisted
/// state plus the two transient overrides. Pure; exported for the unit
/// suite and for `Desk.tsx`'s layout effect, which is the only caller.
export function effectiveCollapsed(
  state: DeskState,
  region: DeskRegionId,
  chrome: DeskChrome,
  zoom: DeskFocusRegion | null,
): boolean {
  // Zoom wins: exactly one region is visible, everything else is at its
  // stripe. Zooming a region obviously does not collapse that region.
  if (zoom !== null) return zoom !== (region as DeskFocusRegion);
  if (chrome !== "full") return true;
  return state.regions[region].collapsed;
}

export function useDesk(repo: string): DeskApi {
  const [state, dispatch] = useReducer(deskReducer, repo, (r) => loadDeskState(r));
  const [drawer, drawerDispatch] = useReducer(drawerSetsReducer, initialDrawerSets);
  const [chrome, setChrome] = useState<DeskChrome>("full");
  const [zoom, setZoom] = useState<DeskFocusRegion | null>(null);
  const [focusRegion, setFocusRegion] = useState<DeskFocusRegion>("main");
  const [resizeMode, setResizeMode] = useState(false);

  // Repo switch: re-seed from THAT repo's stored desk. A ref-guard, not a
  // `key` remount, so the reader's own state (open file, scroll, working
  // set) is untouched — same posture `useWorkingSet` already takes.
  const lastRepo = useRef(repo);
  /// Set on the commit where `repo` changes, cleared by the write-through
  /// effect below. Without it, that effect would fire ONCE with the NEW
  /// repo and the OLD repo's still-unreplaced state — persisting repo A's
  /// desk under repo B's key. (The re-seed below dispatches, and a
  /// dispatch only schedules a re-render; it does not retroactively
  /// change what a later effect in the SAME commit reads.)
  const pendingReseed = useRef(false);
  useEffect(() => {
    if (lastRepo.current === repo) return;
    lastRepo.current = repo;
    pendingReseed.current = true;
    const next = loadDeskState(repo);
    dispatch({ type: "setPreset", preset: next.preset });
    for (const region of ["dock", "rail", "drawer"] as const) {
      dispatch({ type: "resize", target: region, size: next.regions[region].size });
      dispatch({ type: next.regions[region].collapsed ? "collapse" : "expand", region });
    }
    dispatch({ type: "resize", target: "panes", size: next.panes.split });
    dispatch({ type: "setTab", tab: next.railTab });
    if (next.dirty) dispatch({ type: "markDirty" });
  }, [repo]);

  // V70-A4 — `?desk=<preset>`, the A0 grammar's first consumer
  // (`lib/deskParam.ts`). A ONE-SHOT override: applied when the param
  // appears or changes, never re-applied on every render, so an
  // operator who lands on `?desk=review` and then drags a separator
  // keeps their drag. `legacy` is not a preset — `app.tsx` resolves that
  // one by mounting the pre-Desk reader instead, and it never reaches
  // here.
  const override = useDeskOverride();
  const lastOverride = useRef<string | null>(null);
  useEffect(() => {
    if (override === null || override === "legacy") return;
    if (lastOverride.current === override) return;
    lastOverride.current = override;
    // Explicit, not implicit: a URL a human pasted IS a deliberate ask,
    // so it wins over a dirty desk (see `DeskState.dirty`).
    dispatch({ type: "setPreset", preset: override });
  }, [override]);

  // Write-through. Cheap (one small JSON blob) and idempotent, so no
  // debounce: `onLayoutChanged` already fires only on pointer RELEASE,
  // which is the library's own documented "recommended when saving
  // layouts to some storage api" callback.
  useEffect(() => {
    if (pendingReseed.current) {
      // The re-seed's own dispatches will land in the next commit and
      // bring this effect back with the matching state.
      pendingReseed.current = false;
      return;
    }
    saveDeskState(repo, state);
  }, [repo, state]);

  const toggleRegion = useCallback(
    (region: DeskRegionId) => {
      dispatch({ type: state.regions[region].collapsed ? "expand" : "collapse", region, user: true });
    },
    [state.regions],
  );

  const applyPreset = useCallback((preset: DeskPresetName, opts?: { implicit?: boolean }) => {
    dispatch({ type: "setPreset", preset, implicit: opts?.implicit });
  }, []);

  const setRailTab = useCallback((tab: RailTab) => dispatch({ type: "setTab", tab }), []);

  const nudge = useCallback(
    (target: ResizeTarget, delta: number) => {
      const cur = target === "panes" ? state.panes.split : state.regions[target].size;
      // A nudge on a collapsed region expands it first — otherwise the
      // key appears to do nothing and the human has to find the stripe.
      if (target !== "panes" && state.regions[target].collapsed && delta > 0) {
        dispatch({ type: "expand", region: target, user: true });
        return;
      }
      dispatch({ type: "resize", target, size: cur + delta, user: true });
    },
    [state.panes.split, state.regions],
  );

  const equalise = useCallback(() => dispatch({ type: "reset" }), []);

  const toggleZoom = useCallback(
    (region?: DeskFocusRegion) => {
      setZoom((z) => (z === null ? (region ?? "main") : null));
    },
    [],
  );

  const keepInDrawer = useCallback(
    (set: DrawerSetInput) => {
      drawerDispatch({ type: "keep", set });
      // Keeping a set is an explicit ask to SEE it — expanding the
      // drawer is the whole point of the gesture, and it counts as a
      // human layout change (so a preset never yanks it shut later).
      dispatch({ type: "expand", region: "drawer", user: true });
    },
    [],
  );

  const fontScale = chrome === "present" ? DESK_PRESETS.present.fontScale : DESK_PRESETS[state.preset].fontScale;

  // V70-A10 — see `DeskApi.restore`'s own doc.
  const restore = useCallback((next: DeskState) => dispatch({ type: "restore", state: next }), []);

  return useMemo(
    () => ({
      state,
      dispatch,
      chrome,
      setChrome,
      zoom,
      toggleZoom,
      focusRegion,
      setFocusRegion,
      resizeMode,
      setResizeMode,
      toggleRegion,
      applyPreset,
      setRailTab,
      nudge,
      equalise,
      drawer,
      drawerDispatch,
      keepInDrawer,
      dirty: state.dirty,
      fontScale,
      restore,
    }),
    [
      state,
      chrome,
      zoom,
      toggleZoom,
      focusRegion,
      resizeMode,
      toggleRegion,
      applyPreset,
      setRailTab,
      nudge,
      equalise,
      drawer,
      keepInDrawer,
      fontScale,
      restore,
    ],
  );
}

/// The Read preset as a plain value — `Desk.tsx` uses it for the
/// `defaultSize` props (which is what the library resolves a separator
/// double-click against) when no preset is active for some reason.
export const READ_PRESET_STATE = presetState("read");
