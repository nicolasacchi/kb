// V76-R2c — collapse-on-tick.
//
// Ticking a hunk viewed (K2a's per-hunk mark) or a FILE viewed collapses
// that section; un-ticking expands. The two existing per-file controls
// (the chevron / `x`, and the viewed checkbox) stay where they are — this
// module is the extra derived collapse, not a consolidation.
//
// Viewed is SERVER state. Collapse-from-viewed is derived from it. The
// exception — a viewed section the operator re-expanded without un-ticking
// — lives in the URL (`?expanded=` file paths, `?hexpanded=` hunk ids),
// because the review diff's rule is that the URL is the only view state.
// A reload of a viewed file that was not explicitly expanded collapses it
// again.
//
// `folded` (hunk `z c`) and `userCollapsed` (file `x`) stay independent
// user folds. Noise-dial collapse is unchanged. Precedence for the WHY
// chip: fold > noise > viewed.

export type CollapseReason = "fold" | "noise" | "viewed" | null;

export interface HunkCollapseInput {
  viewed: boolean;
  folded: boolean;
  byNoise: boolean;
  /// Present in `?hexpanded=` — a viewed hunk the operator opened again.
  expanded: boolean;
}

export function hunkCollapse(input: HunkCollapseInput): { collapsed: boolean; collapsedBy: CollapseReason } {
  if (input.folded) return { collapsed: true, collapsedBy: "fold" };
  if (input.byNoise) return { collapsed: true, collapsedBy: "noise" };
  if (input.viewed && !input.expanded) return { collapsed: true, collapsedBy: "viewed" };
  return { collapsed: false, collapsedBy: null };
}

export interface FileCollapseInput {
  viewed: boolean;
  userCollapsed: boolean;
  expanded: boolean;
}

export function fileCollapse(input: FileCollapseInput): { collapsed: boolean; collapsedBy: CollapseReason } {
  if (input.userCollapsed) return { collapsed: true, collapsedBy: "fold" };
  if (input.viewed && !input.expanded) return { collapsed: true, collapsedBy: "viewed" };
  return { collapsed: false, collapsedBy: null };
}

/// Toggle the displayed collapse of one section. Expanding a viewed
/// section records an override; collapsing a viewed-and-expanded section
/// drops the override. The `folded`/`userCollapsed` bit is the existing
/// chevron/`x`/`z c` control, returned so the caller writes both stores.
export function toggleSectionCollapse(input: {
  collapsed: boolean;
  viewed: boolean;
  expanded: boolean;
}): { expanded: boolean; userCollapsed: boolean } {
  if (input.collapsed) {
    return { expanded: input.viewed ? true : input.expanded, userCollapsed: false };
  }
  return { expanded: false, userCollapsed: true };
}

export interface CollapseTickState {
  /// Viewed-but-expanded FILE paths.
  expandedFiles: ReadonlySet<string>;
  /// Viewed-but-expanded hunk ids (`kbc-hunkid/1`).
  expandedHunks: ReadonlySet<string>;
}

export type CollapseTickAction =
  | { type: "set"; files: readonly string[]; hunks: readonly string[] }
  | { type: "markViewed"; kind: "file" | "hunk"; id: string }
  | { type: "markUnviewed"; kind: "file" | "hunk"; id: string }
  | { type: "toggleSection"; kind: "file" | "hunk"; id: string; viewed: boolean; collapsed: boolean }
  | { type: "collapseAllViewed"; fileIds: readonly string[]; hunkIds: readonly string[] }
  | { type: "expandAll"; fileIds: readonly string[]; hunkIds: readonly string[] };

function withSet(set: ReadonlySet<string>, id: string, present: boolean): Set<string> {
  if (set.has(id) === present) return new Set(set);
  const next = new Set(set);
  if (present) next.add(id);
  else next.delete(id);
  return next;
}

export function emptyCollapseTick(): CollapseTickState {
  return { expandedFiles: new Set(), expandedHunks: new Set() };
}

export function reduceCollapseTick(state: CollapseTickState, action: CollapseTickAction): CollapseTickState {
  switch (action.type) {
    case "set":
      return {
        expandedFiles: new Set(action.files),
        expandedHunks: new Set(action.hunks),
      };
    case "markViewed": {
      // Ticking viewed collapses: drop any expand override.
      if (action.kind === "file") {
        return { ...state, expandedFiles: withSet(state.expandedFiles, action.id, false) };
      }
      return { ...state, expandedHunks: withSet(state.expandedHunks, action.id, false) };
    }
    case "markUnviewed": {
      // Un-ticking expands; the override is meaningless once unviewed.
      if (action.kind === "file") {
        return { ...state, expandedFiles: withSet(state.expandedFiles, action.id, false) };
      }
      return { ...state, expandedHunks: withSet(state.expandedHunks, action.id, false) };
    }
    case "toggleSection": {
      const next = toggleSectionCollapse({
        collapsed: action.collapsed,
        viewed: action.viewed,
        expanded:
          action.kind === "file" ? state.expandedFiles.has(action.id) : state.expandedHunks.has(action.id),
      });
      if (action.kind === "file") {
        return { ...state, expandedFiles: withSet(state.expandedFiles, action.id, next.expanded) };
      }
      return { ...state, expandedHunks: withSet(state.expandedHunks, action.id, next.expanded) };
    }
    case "collapseAllViewed": {
      const files = new Set(state.expandedFiles);
      const hunks = new Set(state.expandedHunks);
      for (const id of action.fileIds) files.delete(id);
      for (const id of action.hunkIds) hunks.delete(id);
      return { expandedFiles: files, expandedHunks: hunks };
    }
    case "expandAll": {
      const files = new Set(state.expandedFiles);
      const hunks = new Set(state.expandedHunks);
      for (const id of action.fileIds) files.add(id);
      for (const id of action.hunkIds) hunks.add(id);
      return { expandedFiles: files, expandedHunks: hunks };
    }
    default:
      return state;
  }
}

/// TOTAL parser for `?expanded=` / `?hexpanded=`. Empty / null / junk
/// commas → `[]`. Each item is decoded; a broken percent-encoding is kept
/// verbatim rather than throwing.
export function parseExpandedParam(raw: string | null): string[] {
  if (raw == null || raw === "") return [];
  const out: string[] = [];
  for (const part of raw.split(",")) {
    if (part === "") continue;
    try {
      out.push(decodeURIComponent(part));
    } catch {
      out.push(part);
    }
  }
  return out;
}

/// Serialise a set for `?expanded=` / `?hexpanded=`. Empty → `null` (omit
/// the param — the default is "nothing extra-expanded").
export function formatExpandedParam(ids: readonly string[]): string | null {
  if (ids.length === 0) return null;
  return [...ids].sort().map((id) => encodeURIComponent(id)).join(",");
}
