// V70-A4 — `Ctrl-w r`, the keyboard resize submode.
//
// docs/research/kb-code-v7-evidence/research/panel-layout-system.md §3.9
// borrows the tiling-WM shape deliberately: "a transient resize
// **submode** with a floating `?` help, not four modifier chords", and
// §2 records why — "directional ambiguity is a real bug class".
//
// The fix for that bug class is this file's whole reason to exist: a
// direction is resolved against a REGION GRAPH (which region owns the
// boundary that lies that way), never against the focused element's own
// edges. `h` in the rail and `h` in the dock move different separators,
// and the table below is where you read which.
//
// Pure: keys in, commands out. `Desk.tsx` owns the window listener and
// the visible mode chip; nothing here touches the DOM.

import type { ResizeTarget } from "./deskState";

/// Which region currently has the keyboard. `main` covers both panes —
/// the pane split is addressed through `main` + two panes, not through a
/// separate focus value, because the human's mental model is "I am in
/// the code" either way.
export type DeskFocusRegion = "dock" | "main" | "rail" | "drawer";

export type ResizeDir = "left" | "right" | "up" | "down";

/// Percent of the group axis per keypress. `STEP` for `hjkl`, `FAR` for
/// `HJKL` — deliberately not "maximise" for the shifted keys: a
/// reversible big step beats an irreversible jump when there is no
/// layout undo ring yet (that is a Track-A KILL item, panel-layout-
/// system.md §2's `winner-mode` row).
export const RESIZE_STEP = 3;
export const RESIZE_FAR = 15;

export type ResizeSubmodeCommand =
  | { t: "resize"; target: ResizeTarget; delta: number }
  | { t: "equalise" }
  | { t: "hint" }
  | { t: "exit" }
  /// A key the submode does not claim. Swallowed (the submode is modal —
  /// a stray `x` must not fall through to the reader's own keymap) but
  /// otherwise inert.
  | { t: "noop" };

interface GraphEdge {
  target: ResizeTarget;
  /// `+1` grows `target`, `-1` shrinks it.
  sign: 1 | -1;
}

/// Read one row as: "with focus HERE, pressing this direction moves the
/// boundary that way, which means growing/shrinking THAT region."
///
/// `main` is the interesting row. Main has no size of its own (it is the
/// remainder), so a direction from main resolves to whichever neighbour
/// owns the boundary in that direction, with the sign that makes MAIN
/// grow toward the key you pressed:
///   `h` (left)  → main's left edge moves left  → the dock shrinks
///   `l` (right) → main's right edge moves right → the rail shrinks
///   `j` (down)  → main's bottom edge moves down → the drawer shrinks
///   `k` (up)    → main's bottom edge moves up   → the drawer grows
const REGION_GRAPH: Record<DeskFocusRegion, Partial<Record<ResizeDir, GraphEdge>>> = {
  dock: {
    left: { target: "dock", sign: -1 },
    right: { target: "dock", sign: 1 },
  },
  rail: {
    // Mirrored: the rail's boundary is on its LEFT, so pressing left
    // grows it. This asymmetry is the "directional ambiguity" the
    // research names — writing it down as a table is the fix.
    left: { target: "rail", sign: 1 },
    right: { target: "rail", sign: -1 },
  },
  drawer: {
    up: { target: "drawer", sign: 1 },
    down: { target: "drawer", sign: -1 },
  },
  main: {
    left: { target: "dock", sign: -1 },
    right: { target: "rail", sign: -1 },
    up: { target: "drawer", sign: 1 },
    down: { target: "drawer", sign: -1 },
  },
};

/// With a second pane open, horizontal keys in main address the PANE
/// SPLIT rather than the docks — that is the boundary the operator is
/// looking at, and it is the one vim's own `Ctrl-w <`/`>` would move.
const MAIN_SPLIT_GRAPH: Partial<Record<ResizeDir, GraphEdge>> = {
  left: { target: "panes", sign: -1 },
  right: { target: "panes", sign: 1 },
};

const DIR_OF_KEY: Record<string, { dir: ResizeDir; far: boolean }> = {
  h: { dir: "left", far: false },
  j: { dir: "down", far: false },
  k: { dir: "up", far: false },
  l: { dir: "right", far: false },
  H: { dir: "left", far: true },
  J: { dir: "down", far: true },
  K: { dir: "up", far: true },
  L: { dir: "right", far: true },
  ArrowLeft: { dir: "left", far: false },
  ArrowDown: { dir: "down", far: false },
  ArrowUp: { dir: "up", far: false },
  ArrowRight: { dir: "right", far: false },
};

export interface ResizeSubmodeCtx {
  focus: DeskFocusRegion;
  /// Panes the reader is ACTUALLY rendering (URL-derived), not the
  /// desk's remembered intent — see `DeskPanesState.count`'s doc.
  paneCount: 1 | 2;
}

/// One keypress in the submode. Total: every key returns a command, and
/// the caller swallows the event for all of them (the submode is modal).
export function resizeSubmodeKey(key: string, ctx: ResizeSubmodeCtx): ResizeSubmodeCommand {
  if (key === "Escape" || key === "q" || key === "Enter") return { t: "exit" };
  if (key === "=" || key === "+") return { t: "equalise" };
  if (key === "?") return { t: "hint" };

  const mapped = DIR_OF_KEY[key];
  if (!mapped) return { t: "noop" };

  const graph =
    ctx.focus === "main" && ctx.paneCount === 2
      ? { ...REGION_GRAPH.main, ...MAIN_SPLIT_GRAPH }
      : REGION_GRAPH[ctx.focus];
  const edge = graph[mapped.dir];
  // A direction with no boundary that way (e.g. `k` while focused in the
  // dock) is a no-op, not a guess at the nearest other separator.
  if (!edge) return { t: "noop" };

  const magnitude = mapped.far ? RESIZE_FAR : RESIZE_STEP;
  return { t: "resize", target: edge.target, delta: edge.sign * magnitude };
}

/// The one-line `?` hint. Kept here (not in the component) so the text
/// and the grammar above can never drift.
export const RESIZE_SUBMODE_HINT =
  "resize: h/j/k/l step · H/J/K/L far · = equalise · Esc or q exit";
