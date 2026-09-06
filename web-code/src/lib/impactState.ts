// Pure helpers for the impact panel (V3.1-H3b). Network I/O lives in Reader;
// this module flattens wire buckets into navigable rows with section headers.

import type {
  ImpactAnalysisOut,
  ImpactBucketProvenance,
  ImpactRow,
  ImpactTruncated,
} from "../api/types";

export type ImpactBucketId =
  | "direct_exact"
  | "direct_likely"
  | "transitive"
  | "imports"
  | "tests";

/** Display order — Tests always last (blast-proofness bucket). */
export const IMPACT_BUCKET_ORDER: readonly ImpactBucketId[] = [
  "direct_exact",
  "direct_likely",
  "transitive",
  "imports",
  "tests",
] as const;

export const IMPACT_BUCKET_LABEL: Record<ImpactBucketId, string> = {
  direct_exact: "Direct (exact)",
  direct_likely: "Direct (likely)",
  transitive: "Transitive",
  imports: "Imports",
  tests: "Tests",
};

export interface ImpactFlatRow {
  id: string;
  /** Section header row (not navigable). */
  section?: ImpactBucketId;
  /** Truncation sentinel. */
  truncatedNote?: string;
  path: string;
  line: number;
  col: number;
  class: string;
  kind: string;
  name?: string | null;
  depth?: number | null;
  /** Transitive depth-group label rows. */
  depthGroup?: number;
}

export interface ImpactState {
  open: boolean;
  title: string;
  note: string;
  loading: boolean;
  error: string | null;
  data: ImpactAnalysisOut | null;
  /** Collapsed bucket ids (default: none collapsed). */
  collapsed: Set<ImpactBucketId>;
  flat: ImpactFlatRow[];
  cursor: number;
}

export const initialImpactState: ImpactState = {
  open: false,
  title: "",
  note: "",
  loading: false,
  error: null,
  data: null,
  collapsed: new Set(),
  flat: [],
  cursor: 0,
};

function clampCursor(cursor: number, n: number): number {
  if (n === 0) return 0;
  return Math.min(Math.max(cursor, 0), n - 1);
}

function rowsForBucket(data: ImpactAnalysisOut, id: ImpactBucketId): ImpactRow[] {
  switch (id) {
    case "direct_exact":
      return data.direct_exact;
    case "direct_likely":
      return data.direct_likely;
    case "transitive":
      return data.transitive;
    case "imports":
      return data.imports;
    case "tests":
      return data.tests;
  }
}

function truncatedFor(t: ImpactTruncated, id: ImpactBucketId): boolean {
  return t[id];
}

export function provenanceFor(
  data: ImpactAnalysisOut,
  id: ImpactBucketId,
): ImpactBucketProvenance | null | undefined {
  if (!data.provenance) return null;
  if (id === "direct_exact") return data.provenance.direct_exact ?? null;
  if (id === "direct_likely") return data.provenance.direct_likely ?? null;
  return null;
}

/** Flatten buckets into navigable rows; transitive grouped by depth. */
export function flattenImpact(
  data: ImpactAnalysisOut,
  collapsed: Set<ImpactBucketId>,
): ImpactFlatRow[] {
  const out: ImpactFlatRow[] = [];
  let seq = 0;
  const nid = (prefix: string) => {
    seq += 1;
    return `${prefix}-${seq}`;
  };

  for (const bucket of IMPACT_BUCKET_ORDER) {
    const rows = rowsForBucket(data, bucket);
    out.push({
      id: nid("sec"),
      section: bucket,
      path: "",
      line: 0,
      col: 0,
      class: "exact",
      kind: "section",
    });
    if (collapsed.has(bucket)) continue;

    if (bucket === "transitive") {
      // Group by depth (ascending); stable within depth by path:line.
      const byDepth = new Map<number, ImpactRow[]>();
      for (const r of rows) {
        const d = r.depth ?? 1;
        const list = byDepth.get(d) ?? [];
        list.push(r);
        byDepth.set(d, list);
      }
      const depths = [...byDepth.keys()].sort((a, b) => a - b);
      for (const d of depths) {
        out.push({
          id: nid("depth"),
          depthGroup: d,
          path: "",
          line: 0,
          col: 0,
          class: "candidate",
          kind: "depth-group",
        });
        const group = byDepth.get(d)!;
        group.sort((a, b) => a.path.localeCompare(b.path) || a.line - b.line);
        for (const r of group) {
          out.push({
            id: nid("row"),
            path: r.path,
            line: r.line,
            col: r.col,
            class: r.class || "candidate",
            kind: r.kind,
            name: r.name,
            depth: r.depth ?? d,
          });
        }
      }
    } else {
      for (const r of rows) {
        out.push({
          id: nid("row"),
          path: r.path,
          line: r.line,
          col: r.col,
          class: r.class || "candidate",
          kind: r.kind,
          name: r.name,
          depth: r.depth,
        });
      }
    }

    if (truncatedFor(data.truncated, bucket)) {
      out.push({
        id: nid("trunc"),
        truncatedNote: "+more, truncated (server cap)",
        path: "",
        line: 0,
        col: 0,
        class: "candidate",
        kind: "truncated",
      });
    }
  }
  return out;
}

export function bucketCount(data: ImpactAnalysisOut, id: ImpactBucketId): number {
  return rowsForBucket(data, id).length;
}

export type ImpactAction =
  | { type: "OPEN"; title: string }
  | { type: "SET_DATA"; data: ImpactAnalysisOut }
  | { type: "SET_ERROR"; message: string }
  | { type: "MOVE"; delta: number }
  | { type: "TOGGLE_BUCKET"; bucket: ImpactBucketId }
  | { type: "CLOSE" };

export function impactReducer(state: ImpactState, action: ImpactAction): ImpactState {
  switch (action.type) {
    case "OPEN":
      return {
        ...initialImpactState,
        open: true,
        loading: true,
        title: action.title,
        collapsed: new Set(),
      };
    case "SET_DATA": {
      const flat = flattenImpact(action.data, state.collapsed);
      return {
        ...state,
        loading: false,
        error: null,
        data: action.data,
        note: action.data.note,
        title: action.data.symbol.name || state.title,
        flat,
        cursor: 0,
      };
    }
    case "SET_ERROR":
      return {
        ...state,
        loading: false,
        error: action.message,
        data: null,
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
    case "TOGGLE_BUCKET": {
      if (!state.data) return state;
      const collapsed = new Set(state.collapsed);
      if (collapsed.has(action.bucket)) collapsed.delete(action.bucket);
      else collapsed.add(action.bucket);
      const flat = flattenImpact(state.data, collapsed);
      return {
        ...state,
        collapsed,
        flat,
        cursor: clampCursor(state.cursor, flat.length),
      };
    }
    case "CLOSE":
      return state.open ? { ...initialImpactState } : state;
    default:
      return state;
  }
}

export function currentImpactRow(state: ImpactState): ImpactFlatRow | null {
  return state.flat[state.cursor] ?? null;
}

/** True when the row is a real navigable hit (has path:line). */
export function isImpactNavigable(row: ImpactFlatRow): boolean {
  return !!row.path && row.line > 0 && !row.section && !row.truncatedNote && row.depthGroup == null;
}
