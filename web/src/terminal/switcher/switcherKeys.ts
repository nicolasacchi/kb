// TERMSWITCH — next/previous navigation and the chord grammar, pure.

export type SwitcherChord = "next" | "prev" | "needs";

export interface ChordEvent {
  altKey: boolean;
  ctrlKey: boolean;
  metaKey: boolean;
  shiftKey: boolean;
  isComposing?: boolean;
  /** Physical key: layout- and Option-dead-key-independent (macOS Alt+N types a glyph). */
  code: string;
}

/**
 * Alt+ArrowDown = next, Alt+ArrowUp = previous, Alt+N = next needs-you. Alt
 * alone: Ctrl, Meta and Shift must all be off, and a bare key never matches.
 */
export function chordOf(e: ChordEvent): SwitcherChord | null {
  if (!e.altKey || e.ctrlKey || e.metaKey || e.shiftKey || e.isComposing === true) return null;
  if (e.code === "ArrowDown") return "next";
  if (e.code === "ArrowUp") return "prev";
  if (e.code === "KeyN") return "needs";
  return null;
}

/**
 * The id after/before `currentId`, wrapping. An unknown current id starts at
 * the first (next) or last (prev) entry. Null when there is nowhere else to go.
 */
export function nextId(ids: readonly string[], currentId: string | null, dir: 1 | -1): string | null {
  if (ids.length === 0) return null;
  const i = currentId === null ? -1 : ids.indexOf(currentId);
  const n = ids.length;
  const to = i === -1 ? (dir === 1 ? 0 : n - 1) : (i + dir + n) % n;
  return ids[to] === currentId ? null : ids[to];
}

/** The next entry after `currentId` (wrapping) that needs the operator; never the current one. */
export function nextNeedsId(
  rows: readonly { id: string; needs: boolean }[],
  currentId: string | null,
): string | null {
  const n = rows.length;
  const i = currentId === null ? -1 : rows.findIndex((r) => r.id === currentId);
  for (let k = 1; k <= n; k++) {
    const r = rows[(i + k + n) % n];
    if (r.needs && r.id !== currentId) return r.id;
  }
  return null;
}
