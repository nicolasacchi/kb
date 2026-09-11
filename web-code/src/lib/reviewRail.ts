// V76-R2a — the Review Room's findings-rail geometry: a pure, persisted
// reducer, in `desk/deskState.ts`'s exact discipline.
//
// ## The Desk's three rules, applied to the Room
//
//  1. **`react-resizable-panels` is the MECHANISM, never the truth.** The
//     Room body's Group gets its `defaultLayout` from this module at mount
//     and imperative `panelRef.resize()` afterwards; sizes come back IN only
//     through `onLayoutChanged`'s `isUserInteraction` branch. No
//     `autoSaveId` — an opaque localStorage blob that `Space I`
//     (`review.rail.reset`) could not address.
//  2. **Percent, not px.** The Layout map speaks percentages, so this
//     reducer does too (`deskState.ts`'s own ruling — "no conversion happens
//     anywhere between this reducer and the library"). The pixel floors are
//     the Panel's `minSize` props; the reducer's MIN/MAX are the percentage
//     clamp the keyboard submode and the persistence layer share.
//  3. **No second resizer.** Keyboard resize goes THROUGH
//     `desk/resizeSubmode.ts`'s `resizeSubmodeKey` with `focus: "rail"` —
//     the rail row of its REGION_GRAPH (left grows the rail, right shrinks
//     it) is exactly the boundary the Room's one separator owns. This module
//     only translates the submode's command into a width delta.
//
// ## Purity contract
//
// Nothing here reads `window`/`localStorage` directly — `loadReviewRail`/
// `saveReviewRail` take an explicit `StorageLike` (`deskState.ts`'s shape)
// so the node unit suite exercises every branch, corrupt blob included.

import { resizeSubmodeKey, type ResizeSubmodeCommand } from "../desk/resizeSubmode";
import type { StorageLike } from "../desk/deskState";

/// Bumped only when a shape change cannot be forward-migrated silently.
export const REVIEW_RAIL_STATE_VERSION = 1;

/// Percent of the Room body's width. The default mirrors the pre-V76 grid's
/// 280px-on-~1100px column (≈25%); MIN keeps the two-line finding row
/// (slug · path:line) un-crushed, MAX keeps the main column readable.
export const REVIEW_RAIL_DEFAULT_WIDTH = 25;
export const REVIEW_RAIL_MIN_WIDTH = 15;
export const REVIEW_RAIL_MAX_WIDTH = 45;

export interface ReviewRailState {
  /// Kept even while `collapsed` is true: collapsing must be reversible to
  /// the width the human last chose (`deskState.ts`'s own rule, verbatim).
  width: number;
  collapsed: boolean;
}

export const REVIEW_RAIL_DEFAULT: ReviewRailState = {
  width: REVIEW_RAIL_DEFAULT_WIDTH,
  collapsed: false,
};

/// Browser-local by the same D16 ruling every other reading toggle here
/// takes — a rail width is a body memory, not a property of the corpus.
/// ONE key for every review (unlike the desk's per-repo key): the Room's
/// rail is the same surface on every review.
export const REVIEW_RAIL_STORAGE_KEY = "kbc:review-rail";

/// Total coercion: any unknown stored width lands inside [MIN, MAX], and a
/// non-finite one lands on the default.
export function clampRailWidth(width: unknown): number {
  if (typeof width !== "number" || !Number.isFinite(width)) return REVIEW_RAIL_DEFAULT_WIDTH;
  return Math.min(REVIEW_RAIL_MAX_WIDTH, Math.max(REVIEW_RAIL_MIN_WIDTH, width));
}

/// A drag/keyboard delta in percentage points. Resizing a COLLAPSED rail is
/// a no-op — the expand affordance is the stripe, and silently un-collapsing
/// on a stray keypress would be a state change nobody asked for.
export function applyRailResize(state: ReviewRailState, deltaPct: number): ReviewRailState {
  if (state.collapsed) return state;
  return { ...state, width: clampRailWidth(state.width + deltaPct) };
}

/// `Space I` (`review.rail.reset`) and the separator's `=` both land here:
/// back to the default width, and un-collapsed (a reset you cannot see is
/// not a reset).
export function resetRail(): ReviewRailState {
  return { width: REVIEW_RAIL_DEFAULT_WIDTH, collapsed: false };
}

export function toggleRail(state: ReviewRailState): ReviewRailState {
  return { ...state, collapsed: !state.collapsed };
}

export interface RailKeyResult {
  state: ReviewRailState;
  /// The submode's own verdict, handed back so the caller can act on
  /// `exit` (blur the separator) and `hint` the same way the Desk does.
  command: ResizeSubmodeCommand;
  /// Whether the key was one the submode claims — the caller
  /// `preventDefault`/`stopPropagation`s ONLY when this is true (the
  /// focused-panel rule: stop only the keys you handle).
  handled: boolean;
}

/// One keypress on the Room's focused separator, routed through the Desk's
/// resize submode (`focus: "rail"`, `paneCount: 1` — the Room has no pane
/// split, so the main/panes branch is unreachable). `=` maps the submode's
/// `equalise` to THIS surface's honest equivalent: the default width (there
/// is no second region to equalise against).
export function railKeyResize(state: ReviewRailState, key: string): RailKeyResult {
  const command = resizeSubmodeKey(key, { focus: "rail", paneCount: 1 });
  if (command.t === "resize" && command.target === "rail") {
    return { state: applyRailResize(state, command.delta), command, handled: true };
  }
  if (command.t === "equalise") {
    return { state: resetRail(), command, handled: true };
  }
  if (command.t === "exit" || command.t === "hint") {
    return { state, command, handled: true };
  }
  return { state, command, handled: false };
}

function defaultStorage(): StorageLike | null {
  try {
    return typeof localStorage !== "undefined" ? localStorage : null;
  } catch {
    return null;
  }
}

/// Total load: absent key, unreadable storage, corrupt JSON and a
/// wrong-shaped blob ALL degrade to the default — never a throw, never a
/// half-applied state (`loadDeskState`'s own ladder).
export function loadReviewRail(storage?: StorageLike | null): ReviewRailState {
  const store = storage === undefined ? defaultStorage() : storage;
  if (!store) return REVIEW_RAIL_DEFAULT;
  let raw: string | null = null;
  try {
    raw = store.getItem(REVIEW_RAIL_STORAGE_KEY);
  } catch {
    return REVIEW_RAIL_DEFAULT;
  }
  if (raw === null) return REVIEW_RAIL_DEFAULT;
  let parsed: unknown;
  try {
    parsed = JSON.parse(raw);
  } catch {
    return REVIEW_RAIL_DEFAULT;
  }
  if (typeof parsed !== "object" || parsed === null) return REVIEW_RAIL_DEFAULT;
  const p = parsed as { width?: unknown; collapsed?: unknown };
  return {
    width: clampRailWidth(p.width),
    collapsed: p.collapsed === true,
  };
}

export function saveReviewRail(state: ReviewRailState, storage?: StorageLike | null): void {
  const store = storage === undefined ? defaultStorage() : storage;
  if (!store) return;
  try {
    store.setItem(
      REVIEW_RAIL_STORAGE_KEY,
      JSON.stringify({ v: REVIEW_RAIL_STATE_VERSION, width: state.width, collapsed: state.collapsed }),
    );
  } catch {
    // Quota/private-mode refusal — geometry is a nicety, never a crash.
  }
}

/// The Group's `defaultLayout`: plain numbers as PERCENTAGES, keyed by the
/// panels' `id`s — the same map shape `desk/Desk.tsx`'s `colsLayout`
/// returns.
export function roomLayout(railWidth: number): Record<string, number> {
  const w = clampRailWidth(railWidth);
  return { "room-main": 100 - w, "room-rail": w };
}

/// Read the rail's percent back out of a Layout map on a user drag
/// (`onColsLayout`'s own shape). `null` when the map does not carry the
/// rail panel, so the caller keeps its own state rather than persisting
/// junk.
export function railWidthFromLayout(layout: Record<string, unknown>): number | null {
  const raw = layout["room-rail"];
  if (typeof raw === "number" && Number.isFinite(raw)) return clampRailWidth(raw);
  return null;
}
