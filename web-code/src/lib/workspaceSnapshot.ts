// V70-A10 ("Workspaces v0", D26) — the `desk_json` sidecar a workspace
// carries: the persisted `DeskState` (regions/panes/preset/rail tab —
// `deskState.ts`) plus the two things `DeskState` deliberately does NOT
// own (`useDesk.ts`'s own header doc, point 3: "the drawer's result-set
// ring — in-memory only") — the drawer's open tab NAMES (not their row
// data, which is ephemeral search/usages/etc. output with no stable
// "resume" contract) and each working-set entry's captured cursor line.
// Pure + React/CM6-free, so the round trip is unit-tested without a DOM —
// same posture `deskState.ts`/`lib/annotations.ts` already take.

import { migrateDeskState, type DeskState } from "../desk/deskState";

export interface WorkspaceDrawerTabSnapshot {
  title: string;
  pinned: boolean;
}

/// One open pane at save time — the file it showed and its live cursor
/// line, when there was one. `null` in [`WorkspaceSnapshotV1.pane2`] means
/// no split was open.
export interface WorkspacePaneSnapshot {
  path: string;
  line?: number;
}

export interface WorkspaceSnapshotV1 {
  v: 1;
  desk: DeskState;
  /// Drawer tabs open at save time — a RECORD, not a resume point (see the
  /// module doc: drawer row data is ephemeral and not captured).
  drawerTabs: WorkspaceDrawerTabSnapshot[];
  /// Per-path cursor line, captured only for a working-set entry that was
  /// open in one of the two panes at save time — every other entry has no
  /// live cursor to capture and restores as a whole-file span. Mirrors
  /// `pane1`/`pane2` below (the SAME two captured lines, keyed by path
  /// instead of by pane) — kept as its OWN field because it's also what
  /// feeds each SAVED SPAN's own `line_start` on the wire, independent of
  /// which pane a file happened to be in.
  lines: Record<string, number>;
  focusedPane: 1 | 2;
  /// The exact file (+ line) that was in pane 1 at save time — `null` only
  /// when NO file was open at all (an empty working set can still be
  /// "saved," honestly, as a workspace with nothing to jump to). The
  /// PRIMARY thing `~workspaces`' Open action navigates to.
  pane1: WorkspacePaneSnapshot | null;
  /// The file (+ line) that was in pane 2, or `null` when no split was
  /// open. When present, Open restores BOTH panes via `?pane2=`.
  pane2: WorkspacePaneSnapshot | null;
}

export function buildWorkspaceSnapshot(input: {
  desk: DeskState;
  drawerTabs: WorkspaceDrawerTabSnapshot[];
  lines: Record<string, number>;
  focusedPane: 1 | 2;
  pane1: WorkspacePaneSnapshot | null;
  pane2: WorkspacePaneSnapshot | null;
}): WorkspaceSnapshotV1 {
  return {
    v: 1,
    desk: input.desk,
    drawerTabs: input.drawerTabs,
    lines: input.lines,
    focusedPane: input.focusedPane,
    pane1: input.pane1,
    pane2: input.pane2,
  };
}

function isDrawerTabSnapshot(v: unknown): v is WorkspaceDrawerTabSnapshot {
  if (typeof v !== "object" || v === null) return false;
  const o = v as Record<string, unknown>;
  return typeof o.title === "string" && typeof o.pinned === "boolean";
}

function parsePaneSnapshot(v: unknown): WorkspacePaneSnapshot | null {
  if (typeof v !== "object" || v === null) return null;
  const o = v as Record<string, unknown>;
  if (typeof o.path !== "string" || o.path === "") return null;
  const line = typeof o.line === "number" && Number.isFinite(o.line) && o.line > 0 ? o.line : undefined;
  return line !== undefined ? { path: o.path, line } : { path: o.path };
}

/// Parse + forward-migrate an arbitrary `desk_json` string (server-fetched,
/// never trusted) into a [`WorkspaceSnapshotV1`], or `null` on ANY parse
/// failure or unrecognized version — the SAME "never a bricked shell"
/// contract `deskState.ts`'s `migrateDeskState`/`parseDeskJson` document,
/// applied one layer up. `lines` entries that aren't a positive finite
/// number are dropped rather than propagated (a corrupt/foreign blob's
/// bogus line never reaches a `goto`), and so is a malformed `pane1`/
/// `pane2` (degrades to `null` rather than a broken navigation target).
export function parseWorkspaceSnapshot(raw: string): WorkspaceSnapshotV1 | null {
  let parsed: unknown;
  try {
    parsed = JSON.parse(raw);
  } catch {
    return null;
  }
  if (typeof parsed !== "object" || parsed === null) return null;
  const o = parsed as Record<string, unknown>;
  if (o.v !== 1) return null;
  const desk = migrateDeskState(o.desk);
  if (!desk) return null;
  const drawerTabs = Array.isArray(o.drawerTabs) ? o.drawerTabs.filter(isDrawerTabSnapshot) : [];
  const lines: Record<string, number> = {};
  if (typeof o.lines === "object" && o.lines !== null) {
    for (const [k, v] of Object.entries(o.lines as Record<string, unknown>)) {
      if (typeof v === "number" && Number.isFinite(v) && v > 0) lines[k] = v;
    }
  }
  const focusedPane: 1 | 2 = o.focusedPane === 2 ? 2 : 1;
  const pane1 = parsePaneSnapshot(o.pane1);
  const pane2 = parsePaneSnapshot(o.pane2);
  return { v: 1, desk, drawerTabs, lines, focusedPane, pane1, pane2 };
}
