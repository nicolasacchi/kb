// W2.4 — Boards v1: the JSON Canvas (jsoncanvas.org, v1.0) codec + the
// deterministic default layout, kept pure (no React, no fetch, no DOM) —
// same shape as atlasSelection.ts / galleryUrl.ts, colocated vitest
// (canvas.test.ts). BoardCanvas.tsx is the only render-side caller;
// `api/client.ts` only imports the `CanvasDoc` type for its fetch/PUT pair.
//
// Tolerant-read is the whole point: a `.canvas` file is a corpus sidecar
// the daemon stores VERBATIM ("store what parses" — `routes::boards`) and
// other tools (Obsidian, future kb features) may add fields kb doesn't
// know about yet. `parseCanvas` keeps the raw parsed object and only
// ever reads/patches the fields it understands (`nodes`/`edges` arrays,
// each node/edge object as a whole) — it never reconstructs a node/edge
// or the top-level doc field-by-field, so anything unrecognized survives
// a parse → mutate → serialize round-trip untouched.

/** The four JSON Canvas node kinds. kb only ever CREATES `"file"` nodes
 * (list entries) — `"text"`/`"link"`/`"group"` are rendered read-mostly
 * (or passed through) so a board built by hand / another tool doesn't
 * lose content when re-saved from the SPA. */
export type CanvasNodeType = "text" | "file" | "link" | "group";
export type CanvasSide = "top" | "right" | "bottom" | "left";

/** One canvas node. Only `id`/`type`/`x`/`y`/`width`/`height` are
 * required by the spec; everything else is type-specific and optional.
 * The trailing `Record<string, unknown>` intersection keeps any field
 * kb doesn't model (a future JSON Canvas addition, or another tool's
 * extension) intact through a parse/serialize round-trip. */
export type CanvasNode = {
  id: string;
  type: CanvasNodeType;
  x: number;
  y: number;
  width: number;
  height: number;
  color?: string;
  /** `type: "file"` — the artifact's source-relative path (mirrors
   * `ExportEntry.path`/`ListEntry.source_relative`). */
  file?: string;
  /** `type: "file"` — anchors as `#fragment` (a plain `Section{id}`). */
  subpath?: string;
  /** `type: "text"` — Markdown. */
  text?: string;
  /** `type: "link"`. */
  url?: string;
  /** `type: "group"`. */
  label?: string;
} & Record<string, unknown>;

export type CanvasEdge = {
  id: string;
  fromNode: string;
  toNode: string;
  fromSide?: CanvasSide;
  toSide?: CanvasSide;
  fromEnd?: "none" | "arrow";
  toEnd?: "none" | "arrow";
  color?: string;
  label?: string;
} & Record<string, unknown>;

/** A JSON Canvas 1.0 document. Both arrays are optional per spec — a
 * fresh/empty board is `{}` — but every doc this module HANDS BACK from
 * `parseCanvas` always has both as (possibly empty) arrays, so callers
 * never need an `?? []`. */
export type CanvasDoc = {
  nodes: CanvasNode[];
  edges: CanvasEdge[];
} & Record<string, unknown>;

/** The empty board — byte-identical to the daemon's own default
 * (`routes::boards::DEFAULT_CANVAS`), for callers seeding local state
 * before the first GET resolves. */
export const EMPTY_CANVAS: CanvasDoc = { nodes: [], edges: [] };

function isPlainObject(v: unknown): v is Record<string, unknown> {
  return typeof v === "object" && v !== null && !Array.isArray(v);
}

/** Parse an arbitrary JSON value (already `JSON.parse`d, e.g. a fetch
 * response body) into a [`CanvasDoc`]. Tolerant: a non-object, or a
 * missing/non-array `nodes`/`edges`, defaults to `[]` for THAT field
 * only — every other top-level key is kept as-is (spread, not rebuilt).
 * Never throws. */
export function parseCanvas(raw: unknown): CanvasDoc {
  const obj = isPlainObject(raw) ? raw : {};
  const nodes = Array.isArray(obj.nodes) ? (obj.nodes as CanvasNode[]) : [];
  const edges = Array.isArray(obj.edges) ? (obj.edges as CanvasEdge[]) : [];
  return { ...obj, nodes, edges };
}

/** Serialize a [`CanvasDoc`] back to a JSON string for the `PUT` body.
 * Plain `JSON.stringify` — every field `parseCanvas` didn't touch is
 * still sitting on the object, so it round-trips. */
export function serializeCanvas(doc: CanvasDoc): string {
  return JSON.stringify(doc);
}

/** Replace one node's position (drag end / drop-at-center). Only `x`/`y`
 * change — every other field on the node (including anything unknown)
 * is preserved via the object spread. No-op (same array reference isn't
 * guaranteed, but content is unchanged) if `nodeId` isn't found. */
export function moveNode(
  doc: CanvasDoc,
  nodeId: string,
  x: number,
  y: number,
): CanvasDoc {
  return {
    ...doc,
    nodes: doc.nodes.map((n) => (n.id === nodeId ? { ...n, x, y } : n)),
  };
}

/** Append one node (the "place at viewport center" affordance). */
export function addNode(doc: CanvasDoc, node: CanvasNode): CanvasDoc {
  return { ...doc, nodes: [...doc.nodes, node] };
}

/** Remove one node BY ID. Geometry-only — this never touches the
 * reading-list entry the node's `file` may reference (standing rule:
 * "remove node ≠ remove list entry"); dangling edges to/from the
 * removed node are dropped too (an edge to a node that no longer
 * exists isn't a legal JSON Canvas document). */
export function removeNode(doc: CanvasDoc, nodeId: string): CanvasDoc {
  return {
    ...doc,
    nodes: doc.nodes.filter((n) => n.id !== nodeId),
    edges: doc.edges.filter(
      (e) => e.fromNode !== nodeId && e.toNode !== nodeId,
    ),
  };
}

/** The set of source-relative paths already placed as a `"file"` node —
 * `BoardCanvas`'s side tray narrows to entries NOT in this set (multi-
 * placement is a legal outcome of the data model, just not a dedicated
 * UI affordance — recon §3/§4). */
export function placedFiles(doc: CanvasDoc): Set<string> {
  const out = new Set<string>();
  for (const n of doc.nodes) {
    if (n.type === "file" && typeof n.file === "string") out.add(n.file);
  }
  return out;
}

// --- default layout -----------------------------------------------------

/** Fixed card geometry for the default grid layout — matches the size
 * `BoardCanvas` renders a freshly-placed node at, so a board that's
 * never been touched by hand still reads as a tidy grid. */
export const DEFAULT_CARD_WIDTH = 260;
export const DEFAULT_CARD_HEIGHT = 120;
const GRID_GAP = 40;
const GRID_COLUMNS = 4;

export type LayoutEntry = { source_relative?: string | null };

/** Deterministic grid layout for a fresh board: one `"file"` node per
 * entry that has a `source_relative`, in LIST POSITION order (stable —
 * same input order always produces the same node ids/positions), fixed
 * card size, `GRID_COLUMNS` per row. Entries without a resolvable path
 * (a tombstoned artifact) are skipped — there's nothing to open. */
export function defaultLayoutFor(entries: readonly LayoutEntry[]): CanvasDoc {
  const nodes: CanvasNode[] = [];
  let i = 0;
  for (const e of entries) {
    if (!e.source_relative) continue;
    const col = i % GRID_COLUMNS;
    const row = Math.floor(i / GRID_COLUMNS);
    nodes.push({
      id: `n${i}`,
      type: "file",
      x: col * (DEFAULT_CARD_WIDTH + GRID_GAP),
      y: row * (DEFAULT_CARD_HEIGHT + GRID_GAP),
      width: DEFAULT_CARD_WIDTH,
      height: DEFAULT_CARD_HEIGHT,
      file: e.source_relative,
    });
    i += 1;
  }
  return { nodes, edges: [] };
}
