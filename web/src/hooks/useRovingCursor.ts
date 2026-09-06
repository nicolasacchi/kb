import { useCallback, useEffect, useRef, useState } from "react";
import { isEditableTarget } from "../lib/keymap";

// W2.6a — the gallery/search roving cursor. Modeled directly on web-code's
// `components/FileTree.tsx` (the only production roving cursor over a
// virtualized, non-editor DOM list in this repo): a plain `focusedIndex`
// number, clamp-on-rows-change, CSS-class focus (never DOM focus theft),
// and an imperative `scrollToIndex` nudge on every move. Unlike FileTree,
// this owns its OWN window keydown listener (no separate `Reader.tsx`-style
// parent dispatcher exists here) — mirroring the "window handler owns the
// list, an overlay's own listener owns its own keys" split that
// `lib/keymap.ts`'s doc calls the two-layer ownership rule: HotkeyRoot's
// registry machine only ever dispatches `scope: "global"` bindings, so a
// bare j/k/Enter/Escape here never collides with it.
//
// Deliberately NOT built on `lib/keymap.ts`'s chord machine — there's no
// chord/prefix grammar to resolve here, just single-key motions, so a
// second reducer would be pure ceremony. The registry still documents
// these bindings (`scope: "gallery" | "search"`) for the cheat sheet.

export interface UseRovingCursorOptions {
  /// Total number of focusable rows in the flat (already-virtualizer-
  /// flattened) list. Column math is the caller's job via `cols`.
  rows: number;
  /// Column count for a grid layout; 1 (default) for a flat list. j/k/↑/↓
  /// move a full row (`cols` positions); when `cols > 1`, h/l/←/→
  /// additionally move one column at a time.
  cols?: number;
  /// Enter — activate the focused row.
  onActivate: (index: number) => void;
  /// Best-effort virtualized scroll-into-view (e.g. `VirtualGrid`/
  /// `VirtualList`'s forwarded `scrollToIndex`); omit for a non-virtualized
  /// render path.
  scrollToIndex?: (index: number) => void;
  /// Skip wiring the window listener entirely — e.g. a grouped/sectioned
  /// render path is showing instead of the flat virtualized one this cursor
  /// understands, or the view doesn't have a navigable list at all.
  enabled?: boolean;
}

export interface RovingCursor {
  /// -1 = no cursor yet (mouse-only session so far, or cleared by Escape).
  focusedIndex: number;
  setFocusedIndex: (index: number) => void;
  clearFocus: () => void;
}

/// Any of these being open means keyboard input belongs to that overlay,
/// not the roving cursor beneath it — the "overlay owns the keyboard" rule
/// (`PeekPanel`/Cmdk/KeyHelp/ConfirmModal precedent). Generic DOM query
/// rather than plumbing a shared "is anything open" signal through every
/// overlay: every overlay in this codebase already renders one of these
/// two markers (`role="dialog" aria-modal="true"`, or a native `<dialog>`
/// via `showModal()`).
function isOverlayOpen(): boolean {
  return !!document.querySelector('dialog[open], [role="dialog"][aria-modal="true"]');
}

export function useRovingCursor({
  rows,
  cols = 1,
  onActivate,
  scrollToIndex,
  enabled = true,
}: UseRovingCursorOptions): RovingCursor {
  const [focusedIndex, setFocusedIndexState] = useState(-1);

  // Refs mirroring the latest committed values the keydown closure needs —
  // kept out of the effect's deps so the listener doesn't tear down/rebind
  // on every cursor move (same idiom as HotkeyRoot's `kbRef`).
  const onActivateRef = useRef(onActivate);
  onActivateRef.current = onActivate;
  const colsRef = useRef(cols);
  colsRef.current = cols;
  const rowsRef = useRef(rows);
  rowsRef.current = rows;
  const focusedIndexRef = useRef(focusedIndex);
  focusedIndexRef.current = focusedIndex;

  const clamp = useCallback((i: number) => Math.max(0, Math.min(i, Math.max(0, rows - 1))), [rows]);

  // Clamp-on-rows-change (FileTree.tsx:103-105) — a filter/page change that
  // shrinks the list never leaves the cursor pointing past the end; a
  // never-focused cursor (-1) stays untouched (no surprise focus from a
  // background refetch).
  useEffect(() => {
    setFocusedIndexState((i) => (i < 0 ? i : clamp(i)));
  }, [rows, clamp]);

  const setFocusedIndex = useCallback((i: number) => setFocusedIndexState(clamp(i)), [clamp]);
  const clearFocus = useCallback(() => setFocusedIndexState(-1), []);

  useEffect(() => {
    if (!enabled || rows === 0) return;

    const move = (delta: number) => {
      const cur = focusedIndexRef.current < 0 ? 0 : focusedIndexRef.current;
      setFocusedIndexState(Math.max(0, Math.min(cur + delta, Math.max(0, rowsRef.current - 1))));
    };

    function onKey(e: KeyboardEvent) {
      if (e.metaKey || e.ctrlKey || e.altKey) return;
      if (isEditableTarget(e.target)) return;
      if (isOverlayOpen()) return;

      switch (e.key) {
        case "j":
        case "ArrowDown":
          e.preventDefault();
          move(colsRef.current);
          return;
        case "k":
        case "ArrowUp":
          e.preventDefault();
          move(-colsRef.current);
          return;
        case "l":
        case "ArrowRight":
          if (colsRef.current > 1) {
            e.preventDefault();
            move(1);
          }
          return;
        case "h":
        case "ArrowLeft":
          if (colsRef.current > 1) {
            e.preventDefault();
            move(-1);
          }
          return;
        case "Enter":
          if (focusedIndexRef.current >= 0) {
            e.preventDefault();
            onActivateRef.current(focusedIndexRef.current);
          }
          return;
        case "Escape":
          if (focusedIndexRef.current >= 0) {
            e.preventDefault();
            setFocusedIndexState(-1);
          }
          return;
        default:
          return;
      }
    }
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [enabled, rows]);

  useEffect(() => {
    if (focusedIndex >= 0) scrollToIndex?.(focusedIndex);
  }, [focusedIndex, scrollToIndex]);

  return { focusedIndex, setFocusedIndex, clearFocus };
}
