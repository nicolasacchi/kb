// Pure helpers for the call/type hierarchy panel (V3.1-H3a).
// Network I/O lives in `Reader.tsx`; this module shapes tree rows, depth
// caps, cycle detection, and wire → row mapping.

import type {
  HierarchyCalleeSite,
  HierarchyCalleesOut,
  HierarchyCallerGroup,
  HierarchyCallersOut,
  HierarchyTypeEdge,
  HierarchyTypesOut,
} from "../api/types";

/** UI hard cap on expand depth (root = depth 0). */
export const HIERARCHY_DEPTH_CAP = 5;

export type HierarchyMode = "callers" | "callees" | "types";

export type HierarchyDir = "callers" | "callees" | "supertypes" | "subtypes";

export interface HierarchyLoc {
  path: string;
  line: number;
  col: number;
}

export interface HierarchyNode {
  id: string;
  name: string;
  kind?: string;
  path: string;
  line: number;
  col: number;
  class: string;
  depth: number;
  dir: HierarchyDir;
  /** True when this node is an ancestor-cycle (same path:line) — no expand. */
  cycle: boolean;
  /** Depth-cap reached — render as leaf. */
  depthCapped: boolean;
  expanded: boolean;
  loading: boolean;
  children: HierarchyNode[];
  /** Truncation sentinel row (`+N more, truncated`). */
  truncatedNote?: string;
  /** Section label for types mode roots (Supertypes / Subtypes). */
  section?: "supertypes" | "subtypes";
}

export interface HierarchyState {
  open: boolean;
  mode: HierarchyMode;
  title: string;
  loading: boolean;
  error: string | null;
  roots: HierarchyNode[];
  /** Flat list of navigable rows (depth-first, expanded only). */
  flat: HierarchyNode[];
  cursor: number;
}

export const initialHierarchyState: HierarchyState = {
  open: false,
  mode: "callers",
  title: "",
  loading: false,
  error: null,
  roots: [],
  flat: [],
  cursor: 0,
};

let nodeSeq = 0;
function nextId(prefix: string): string {
  nodeSeq += 1;
  return `${prefix}-${nodeSeq}`;
}

export function locKey(path: string, line: number): string {
  return `${path}:${line}`;
}

/** Ancestor path:line keys for cycle detection. */
export function ancestorKeys(chain: HierarchyLoc[]): Set<string> {
  return new Set(chain.map((l) => locKey(l.path, l.line)));
}

export function flattenTree(roots: HierarchyNode[]): HierarchyNode[] {
  const out: HierarchyNode[] = [];
  function walk(n: HierarchyNode) {
    out.push(n);
    if (n.expanded && !n.cycle) {
      for (const c of n.children) walk(c);
    }
  }
  for (const r of roots) walk(r);
  return out;
}

function clampCursor(cursor: number, n: number): number {
  if (n === 0) return 0;
  return Math.min(Math.max(cursor, 0), n - 1);
}

export function rebuildFlat(state: HierarchyState): HierarchyState {
  const flat = flattenTree(state.roots);
  return { ...state, flat, cursor: clampCursor(state.cursor, flat.length) };
}

function makeNode(partial: Omit<HierarchyNode, "id" | "children" | "expanded" | "loading"> & {
  id?: string;
  children?: HierarchyNode[];
  expanded?: boolean;
  loading?: boolean;
}): HierarchyNode {
  return {
    id: partial.id ?? nextId("hn"),
    name: partial.name,
    kind: partial.kind,
    path: partial.path,
    line: partial.line,
    col: partial.col,
    class: partial.class || "candidate",
    depth: partial.depth,
    dir: partial.dir,
    cycle: partial.cycle,
    depthCapped: partial.depthCapped,
    expanded: partial.expanded ?? false,
    loading: partial.loading ?? false,
    children: partial.children ?? [],
    truncatedNote: partial.truncatedNote,
    section: partial.section,
  };
}

/** Map callers wire groups → child nodes under a parent at `depth`. */
export function callersToNodes(
  groups: HierarchyCallerGroup[],
  depth: number,
  dir: HierarchyDir,
  ancestorPathLines: Set<string>,
  truncated: boolean,
): HierarchyNode[] {
  const nodes: HierarchyNode[] = [];
  for (const g of groups) {
    const site = g.sites[0];
    if (!site) continue;
    // Prefer enclosing function identity; fall back to path + first site.
    const name = g.enclosing?.name ?? g.path;
    const line = g.enclosing?.line ?? site.line;
    const col = site.col;
    const class_ = site.class || "candidate";
    const key = locKey(g.path, line);
    const cycle = ancestorPathLines.has(key);
    const depthCapped = depth >= HIERARCHY_DEPTH_CAP;
    nodes.push(
      makeNode({
        name,
        kind: g.enclosing?.kind,
        path: g.path,
        line,
        col,
        class: class_,
        depth,
        dir,
        cycle,
        depthCapped,
      }),
    );
  }
  if (truncated) {
    nodes.push(
      makeNode({
        name: `+more, truncated`,
        path: "",
        line: 0,
        col: 0,
        class: "candidate",
        depth,
        dir,
        cycle: true,
        depthCapped: true,
        truncatedNote: "+more, truncated (server cap)",
      }),
    );
  }
  return nodes;
}

/** Map callees wire sites → child nodes. */
export function calleesToNodes(
  sites: HierarchyCalleeSite[],
  depth: number,
  dir: HierarchyDir,
  ancestorPathLines: Set<string>,
): HierarchyNode[] {
  return sites.map((s) => {
    const path = s.target?.path ?? "";
    const line = s.target?.line ?? s.line;
    const col = s.col;
    const key = path ? locKey(path, line) : locKey(`@${s.line}`, s.col);
    const cycle = ancestorPathLines.has(key);
    const depthCapped = depth >= HIERARCHY_DEPTH_CAP;
    return makeNode({
      name: s.name,
      path: path || "?",
      line,
      col,
      class: s.class || "candidate",
      depth,
      dir,
      cycle,
      depthCapped,
    });
  });
}

export function typeEdgesToNodes(
  edges: HierarchyTypeEdge[],
  depth: number,
  dir: HierarchyDir,
  section: "supertypes" | "subtypes",
  ancestorPathLines: Set<string>,
): HierarchyNode[] {
  return edges.map((e) => {
    const path = e.target?.path ?? e.via.path;
    const line = e.target?.line ?? e.via.line;
    const key = locKey(path, line);
    const cycle = ancestorPathLines.has(key);
    const depthCapped = depth >= HIERARCHY_DEPTH_CAP;
    return makeNode({
      name: e.name,
      kind: e.kind,
      path,
      line,
      col: 0,
      class: e.class || "candidate",
      depth,
      dir,
      cycle,
      depthCapped,
      section,
    });
  });
}

export function rootFromFunction(
  name: string,
  path: string,
  line: number,
  kind: string | undefined,
  mode: HierarchyMode,
): HierarchyNode {
  const dir: HierarchyDir = mode === "callers" ? "callers" : mode === "callees" ? "callees" : "subtypes";
  return makeNode({
    name,
    kind,
    path,
    line,
    col: 0,
    class: "exact",
    depth: 0,
    dir,
    cycle: false,
    depthCapped: false,
    expanded: true,
  });
}

export function buildCallersTree(out: HierarchyCallersOut): HierarchyNode[] {
  const root = rootFromFunction(
    out.function.name,
    out.function.path,
    out.function.line,
    out.function.kind,
    "callers",
  );
  const ancestors = ancestorKeys([{ path: out.function.path, line: out.function.line, col: 0 }]);
  root.children = callersToNodes(out.callers, 1, "callers", ancestors, out.truncated);
  return [root];
}

export function buildCalleesTree(out: HierarchyCalleesOut): HierarchyNode[] {
  const root = rootFromFunction(
    out.function.name,
    out.function.path,
    out.function.line,
    out.function.kind,
    "callees",
  );
  const ancestors = ancestorKeys([{ path: out.function.path, line: out.function.line, col: 0 }]);
  root.children = calleesToNodes(out.callees, 1, "callees", ancestors);
  return [root];
}

export function buildTypesTree(out: HierarchyTypesOut, seedPath?: string): HierarchyNode[] {
  const root = rootFromFunction(out.name, seedPath ?? "", 0, undefined, "types");
  root.class = "exact";
  const ancestors = new Set<string>();
  const supers = typeEdgesToNodes(out.supertypes, 1, "supertypes", "supertypes", ancestors);
  const subs = typeEdgesToNodes(out.subtypes, 1, "subtypes", "subtypes", ancestors);
  // Section header nodes (non-navigable expanders) + edges as children of root.
  const children: HierarchyNode[] = [];
  if (supers.length > 0) {
    children.push(
      makeNode({
        name: "Supertypes",
        path: "",
        line: 0,
        col: 0,
        class: "exact",
        depth: 1,
        dir: "supertypes",
        cycle: false,
        depthCapped: false,
        expanded: true,
        section: "supertypes",
        children: supers,
      }),
    );
  }
  if (subs.length > 0) {
    children.push(
      makeNode({
        name: "Subtypes / implementors",
        path: "",
        line: 0,
        col: 0,
        class: "exact",
        depth: 1,
        dir: "subtypes",
        cycle: false,
        depthCapped: false,
        expanded: true,
        section: "subtypes",
        children: subs,
      }),
    );
  }
  root.children = children;
  return [root];
}

/** Collect path:line ancestors from root down to `nodeId` (inclusive). */
export function collectAncestorLocs(roots: HierarchyNode[], nodeId: string): HierarchyLoc[] | null {
  const chain: HierarchyLoc[] = [];
  function walk(nodes: HierarchyNode[]): boolean {
    for (const n of nodes) {
      chain.push({ path: n.path, line: n.line, col: n.col });
      if (n.id === nodeId) return true;
      if (walk(n.children)) return true;
      chain.pop();
    }
    return false;
  }
  return walk(roots) ? chain : null;
}

/** Replace a node in the tree by id (immutable). */
export function updateNode(
  roots: HierarchyNode[],
  id: string,
  patch: (n: HierarchyNode) => HierarchyNode,
): HierarchyNode[] {
  return roots.map((n) => {
    if (n.id === id) return patch(n);
    if (n.children.length === 0) return n;
    return { ...n, children: updateNode(n.children, id, patch) };
  });
}

export type HierarchyAction =
  | { type: "OPEN"; mode: HierarchyMode; title: string }
  | { type: "SET_TREE"; roots: HierarchyNode[] }
  | { type: "SET_ERROR"; message: string }
  | { type: "MOVE"; delta: number }
  | { type: "SET_CURSOR"; index: number }
  | { type: "PATCH_ROOTS"; roots: HierarchyNode[] }
  | { type: "CLOSE" };

export function hierarchyReducer(state: HierarchyState, action: HierarchyAction): HierarchyState {
  switch (action.type) {
    case "OPEN":
      return {
        ...initialHierarchyState,
        open: true,
        loading: true,
        mode: action.mode,
        title: action.title,
      };
    case "SET_TREE":
      return rebuildFlat({
        ...state,
        loading: false,
        error: null,
        roots: action.roots,
        cursor: 0,
      });
    case "SET_ERROR":
      return {
        ...state,
        loading: false,
        error: action.message,
        roots: [],
        flat: [],
        cursor: 0,
      };
    case "MOVE": {
      if (state.flat.length === 0) return state;
      return {
        ...state,
        cursor: clampCursor(state.cursor + action.delta, state.flat.length),
      };
    }
    case "SET_CURSOR":
      return { ...state, cursor: clampCursor(action.index, state.flat.length) };
    case "PATCH_ROOTS":
      return rebuildFlat({ ...state, roots: action.roots });
    case "CLOSE":
      return state.open ? { ...initialHierarchyState } : state;
    default:
      return state;
  }
}

export function currentHierarchyRow(state: HierarchyState): HierarchyNode | null {
  return state.flat[state.cursor] ?? null;
}

/** Kinds that make sense for type hierarchy (`gt`). */
const TYPE_ISH = new Set([
  "class",
  "struct",
  "enum",
  "trait",
  "interface",
  "type",
  "type_alias",
  "impl",
  "union",
  "protocol",
  "module",
  "namespace",
]);

export function isTypeIshKind(kind: string | null | undefined): boolean {
  if (!kind) return false;
  const k = kind.toLowerCase().replace(/\s+/g, "_");
  return TYPE_ISH.has(k) || k.includes("class") || k.includes("trait") || k.includes("interface") || k.includes("struct");
}
