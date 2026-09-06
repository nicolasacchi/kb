// X2 — Cmd+K flat-cursor geometry. The palette concatenates every visible
// group into ONE keyboard cursor; centralising the offset math here (used by
// BOTH the ↑↓/Enter handler and the render) is what keeps the highlighted row
// and the Enter target from ever disagreeing as groups grow/shrink. Pure +
// unit-tested (cmdkRows.test.ts) so the arithmetic is verified without a DOM.

export type CmdkRowKind =
  | "recent"
  | "command"
  | "hit"
  | "note"
  | "memory"
  | "session";

export type CmdkCounts = {
  recents: number;
  commands: number;
  hits: number;
  notes: number;
  memories: number;
  sessions: number;
};

// The fixed top-to-bottom order the palette renders groups in.
const ORDER: CmdkRowKind[] = [
  "recent",
  "command",
  "hit",
  "note",
  "memory",
  "session",
];

function countOf(kind: CmdkRowKind, c: CmdkCounts): number {
  switch (kind) {
    case "recent":
      return c.recents;
    case "command":
      return c.commands;
    case "hit":
      return c.hits;
    case "note":
      return c.notes;
    case "memory":
      return c.memories;
    case "session":
      return c.sessions;
  }
}

export function cmdkTotalRows(c: CmdkCounts): number {
  return c.recents + c.commands + c.hits + c.notes + c.memories + c.sessions;
}

// The flat index of the FIRST row of `kind` — the base offset a render adds its
// in-group index to.
export function cmdkRowBase(kind: CmdkRowKind, c: CmdkCounts): number {
  let base = 0;
  for (const k of ORDER) {
    if (k === kind) return base;
    base += countOf(k, c);
  }
  return base;
}

// Resolve a flat cursor to its group + in-group index, or null if out of range
// (empty groups are skipped, so the kind is always a non-empty group).
export function cmdkRowAt(
  cursor: number,
  c: CmdkCounts,
): { kind: CmdkRowKind; idx: number } | null {
  if (cursor < 0) return null;
  let base = 0;
  for (const k of ORDER) {
    const n = countOf(k, c);
    if (cursor < base + n) return { kind: k, idx: cursor - base };
    base += n;
  }
  return null;
}
