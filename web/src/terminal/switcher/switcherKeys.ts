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
 * The row after/before `currentId`, wrapping, over the listed (flat) order. An
 * unknown current id starts at the first (next) or last (prev) entry. Null when
 * there is nowhere else to go.
 */
export function nextId(flat: readonly { id: string }[], currentId: string | null, dir: 1 | -1): string | null {
  if (flat.length === 0) return null;
  const i = currentId === null ? -1 : flat.findIndex((r) => r.id === currentId);
  const n = flat.length;
  const to = i === -1 ? (dir === 1 ? 0 : n - 1) : (i + dir + n) % n;
  return flat[to].id === currentId ? null : flat[to].id;
}

/** The next tier-`needs` row after `currentId` (wrapping); never the current one. */
export function nextNeedsId(flat: readonly { id: string; tier: string }[], currentId: string | null): string | null {
  const n = flat.length;
  const i = currentId === null ? -1 : flat.findIndex((r) => r.id === currentId);
  for (let k = 1; k <= n; k++) {
    const r = flat[(i + k + n) % n];
    if (r.tier === "needs" && r.id !== currentId) return r.id;
  }
  return null;
}
