// Pure layered ego-graph layout (V3.1-H3b). No physics, no randomness —
// identical input ⇒ identical output. That determinism IS the golden test.
//
// Layout: callers column (left, layer −1) · center (layer 0) · callees/
// implementors column (right, layer +1). Optional depth-2 columns at
// layers −2 / +2. Vertical stacking with a fixed row pitch; order within
// a layer is class-rank then name.

export type EgoClass = "exact" | "likely" | "candidate" | string;

export interface EgoInputNode {
  id: string;
  name: string;
  class?: EgoClass;
  path?: string;
  line?: number;
  kind?: string;
}

export interface EgoInputEdge {
  from: string;
  to: string;
  class?: EgoClass;
}

export interface EgoLayoutInput {
  center: EgoInputNode;
  /** Incoming edges (callers) — `to` is the center (or a depth-1 node for hop-2). */
  inEdges: EgoInputEdge[];
  /** Outgoing edges (callees/implementors). */
  outEdges: EgoInputEdge[];
  /** Neighbor nodes keyed by id (must cover every edge endpoint except center). */
  nodes: Record<string, EgoInputNode>;
  /** 1 = one hop; 2 = expand outer columns. Default 1. */
  depth?: 1 | 2;
  /** Hard node cap (center counts). Default 40. */
  nodeCap?: number;
}

export interface EgoLaidOutNode {
  id: string;
  name: string;
  class: string;
  path?: string;
  line?: number;
  kind?: string;
  /** −2..+2 layered column index. */
  layer: number;
  x: number;
  y: number;
}

export interface EgoLaidOutEdge {
  from: string;
  to: string;
  class: string;
}

export interface EgoLayoutResult {
  nodes: EgoLaidOutNode[];
  edges: EgoLaidOutEdge[];
  /** How many neighbor nodes were dropped by the cap. */
  truncated: number;
  width: number;
  height: number;
}

/** Class rank: exact first, then likely, then candidate, then other. */
export function classRank(c: string | undefined | null): number {
  const v = (c || "candidate").toLowerCase();
  if (v === "exact") return 0;
  if (v === "likely") return 1;
  if (v === "candidate") return 2;
  return 3;
}

/** Deterministic neighbor order: class-rank ascending, then name, then id. */
export function compareNodes(a: EgoInputNode, b: EgoInputNode): number {
  const cr = classRank(a.class) - classRank(b.class);
  if (cr !== 0) return cr;
  const n = a.name.localeCompare(b.name);
  if (n !== 0) return n;
  return a.id.localeCompare(b.id);
}

// Geometry constants (golden-pinned via layout output).
export const EGO_COL_GAP = 180;
export const EGO_ROW_PITCH = 48;
export const EGO_PAD_X = 24;
export const EGO_PAD_Y = 24;
export const EGO_NODE_CAP_DEFAULT = 40;

/**
 * Pure layered layout. Depth ≤ 2. Node cap includes the center.
 * Callers left of center, callees right; vertical stack centered on the
 * center node's y.
 */
export function layoutEgoGraph(input: EgoLayoutInput): EgoLayoutResult {
  const depth = input.depth === 2 ? 2 : 1;
  const cap = input.nodeCap ?? EGO_NODE_CAP_DEFAULT;
  const center = input.center;

  // Collect unique one-hop neighbors.
  const callerIds = new Set<string>();
  const calleeIds = new Set<string>();
  for (const e of input.inEdges) {
    if (e.to === center.id && e.from !== center.id) callerIds.add(e.from);
    // Depth-2: edges into a depth-1 caller (handled below when depth=2).
  }
  for (const e of input.outEdges) {
    if (e.from === center.id && e.to !== center.id) calleeIds.add(e.to);
  }

  let callers = [...callerIds]
    .map((id) => input.nodes[id] ?? { id, name: id, class: "candidate" })
    .sort(compareNodes);
  let callees = [...calleeIds]
    .map((id) => input.nodes[id] ?? { id, name: id, class: "candidate" })
    .sort(compareNodes);

  // Depth-2 outer neighbors (callers-of-callers / callees-of-callees).
  let outerCallers: EgoInputNode[] = [];
  let outerCallees: EgoInputNode[] = [];
  if (depth === 2) {
    const d1Caller = new Set(callers.map((n) => n.id));
    const d1Callee = new Set(callees.map((n) => n.id));
    const oc = new Set<string>();
    const oe = new Set<string>();
    for (const e of input.inEdges) {
      if (d1Caller.has(e.to) && e.from !== center.id && !d1Caller.has(e.from)) {
        oc.add(e.from);
      }
    }
    for (const e of input.outEdges) {
      if (d1Callee.has(e.from) && e.to !== center.id && !d1Callee.has(e.to)) {
        oe.add(e.to);
      }
    }
    outerCallers = [...oc]
      .map((id) => input.nodes[id] ?? { id, name: id, class: "candidate" })
      .sort(compareNodes);
    outerCallees = [...oe]
      .map((id) => input.nodes[id] ?? { id, name: id, class: "candidate" })
      .sort(compareNodes);
  }

  // Cap: keep center + fill from callers then callees then outer (stable order).
  const budget = Math.max(0, cap - 1);
  let remaining = budget;
  const take = <T,>(arr: T[]): T[] => {
    const n = Math.min(arr.length, remaining);
    remaining -= n;
    return arr.slice(0, n);
  };
  callers = take(callers);
  callees = take(callees);
  outerCallers = take(outerCallers);
  outerCallees = take(outerCallees);

  const kept = 1 + callers.length + callees.length + outerCallers.length + outerCallees.length;
  // Truncated = unique non-center nodes considered minus those kept (excl. center).
  const considered = new Set<string>([...callerIds, ...calleeIds]);
  if (depth === 2) {
    for (const e of input.inEdges) {
      if (callerIds.has(e.to) && e.from !== center.id) considered.add(e.from);
    }
    for (const e of input.outEdges) {
      if (calleeIds.has(e.from) && e.to !== center.id) considered.add(e.to);
    }
  }
  const truncated = Math.max(0, considered.size - (kept - 1));

  // Place columns.
  const layers: { layer: number; nodes: EgoInputNode[] }[] = [];
  if (outerCallers.length) layers.push({ layer: -2, nodes: outerCallers });
  if (callers.length) layers.push({ layer: -1, nodes: callers });
  layers.push({ layer: 0, nodes: [center] });
  if (callees.length) layers.push({ layer: 1, nodes: callees });
  if (outerCallees.length) layers.push({ layer: 2, nodes: outerCallees });

  const maxRows = Math.max(...layers.map((l) => l.nodes.length), 1);
  const height = EGO_PAD_Y * 2 + (maxRows - 1) * EGO_ROW_PITCH + 32;
  const centerY = height / 2;

  // X positions by unique layer values present.
  const layerXs = new Map<number, number>();
  const sortedLayers = [...new Set(layers.map((l) => l.layer))].sort((a, b) => a - b);
  sortedLayers.forEach((layer, i) => {
    layerXs.set(layer, EGO_PAD_X + i * EGO_COL_GAP);
  });
  const width = EGO_PAD_X * 2 + Math.max(0, sortedLayers.length - 1) * EGO_COL_GAP + 120;

  const laid: EgoLaidOutNode[] = [];
  for (const { layer, nodes } of layers) {
    const x = layerXs.get(layer) ?? EGO_PAD_X;
    const n = nodes.length;
    // Vertically center the stack around centerY.
    const stackH = (n - 1) * EGO_ROW_PITCH;
    const y0 = centerY - stackH / 2;
    nodes.forEach((node, i) => {
      laid.push({
        id: node.id,
        name: node.name,
        class: (node.class || (layer === 0 ? "exact" : "candidate")).toLowerCase(),
        path: node.path,
        line: node.line,
        kind: node.kind,
        layer,
        x,
        y: y0 + i * EGO_ROW_PITCH,
      });
    });
  }

  const keptIds = new Set(laid.map((n) => n.id));
  const edges: EgoLaidOutEdge[] = [];
  const edgeKey = (a: string, b: string) => `${a}\0${b}`;
  const seen = new Set<string>();
  for (const e of [...input.inEdges, ...input.outEdges]) {
    if (!keptIds.has(e.from) || !keptIds.has(e.to)) continue;
    const k = edgeKey(e.from, e.to);
    if (seen.has(k)) continue;
    seen.add(k);
    edges.push({
      from: e.from,
      to: e.to,
      class: (e.class || "candidate").toLowerCase(),
    });
  }
  // Deterministic edge order.
  edges.sort((a, b) => a.from.localeCompare(b.from) || a.to.localeCompare(b.to));

  // Deterministic node order: layer then y then id.
  laid.sort((a, b) => a.layer - b.layer || a.y - b.y || a.id.localeCompare(b.id));

  return { nodes: laid, edges, truncated, width, height };
}

// --- V3.3-S1: general layered DAG (review map reuses the same geometry) ---
//
// Ruling D5: ONE pure layout family (no physics). Review-map nodes/edges
// are path-keyed; edge `from → to` means from depends on to (import/call),
// so dependencies sit on lower (left) layers — bottom-up, same as reading
// order.

export interface LayeredDagEdge {
  from: string;
  to: string;
  class?: EgoClass;
  /** `"import"` | `"call"` | … — carried through for stroke styling. */
  kind?: string;
}

export interface LayeredDagInput {
  nodes: EgoInputNode[];
  edges: LayeredDagEdge[];
  /** Hard node cap. Default 80. */
  nodeCap?: number;
}

export interface LayeredDagLaidOutEdge extends EgoLaidOutEdge {
  kind: string;
}

export interface LayeredDagLayoutResult {
  nodes: EgoLaidOutNode[];
  edges: LayeredDagLaidOutEdge[];
  truncated: number;
  width: number;
  height: number;
}

/**
 * Pure longest-path layered layout for an arbitrary DAG.
 * Edge direction: `from` depends on `to` ⇒ `to` is left of `from`.
 * Isolated nodes land on layer 0, path-asc within each layer.
 */
export function layoutLayeredDag(input: LayeredDagInput): LayeredDagLayoutResult {
  const cap = input.nodeCap ?? 80;
  // Stable path-asc input order for ties.
  let nodes = [...input.nodes].sort(
    (a, b) => a.name.localeCompare(b.name) || a.id.localeCompare(b.id),
  );
  const truncated = Math.max(0, nodes.length - cap);
  if (nodes.length > cap) {
    nodes = nodes.slice(0, cap);
  }
  const kept = new Set(nodes.map((n) => n.id));

  // Only edges with both ends kept.
  const edges = input.edges.filter((e) => kept.has(e.from) && kept.has(e.to) && e.from !== e.to);
  // Incoming deps: for each node, the set it depends on (`to`s).
  const deps = new Map<string, string[]>();
  for (const n of nodes) deps.set(n.id, []);
  for (const e of edges) {
    deps.get(e.from)?.push(e.to);
  }

  // Longest-path layering with memo; cycle ⇒ clamp via visited set.
  const layerOf = new Map<string, number>();
  const visiting = new Set<string>();
  function layer(id: string): number {
    const cached = layerOf.get(id);
    if (cached !== undefined) return cached;
    if (visiting.has(id)) {
      layerOf.set(id, 0);
      return 0;
    }
    visiting.add(id);
    let maxDep = -1;
    for (const d of deps.get(id) ?? []) {
      if (!kept.has(d)) continue;
      maxDep = Math.max(maxDep, layer(d));
    }
    visiting.delete(id);
    const L = maxDep + 1;
    layerOf.set(id, L);
    return L;
  }
  for (const n of nodes) layer(n.id);

  // Bucket by layer.
  const buckets = new Map<number, EgoInputNode[]>();
  for (const n of nodes) {
    const L = layerOf.get(n.id) ?? 0;
    const arr = buckets.get(L) ?? [];
    arr.push(n);
    buckets.set(L, arr);
  }
  const sortedLayers = [...buckets.keys()].sort((a, b) => a - b);
  for (const L of sortedLayers) {
    buckets.get(L)!.sort((a, b) => a.name.localeCompare(b.name) || a.id.localeCompare(b.id));
  }

  const maxRows = Math.max(...sortedLayers.map((L) => buckets.get(L)!.length), 1);
  const height = EGO_PAD_Y * 2 + (maxRows - 1) * EGO_ROW_PITCH + 32;
  const centerY = height / 2;
  const layerXs = new Map<number, number>();
  sortedLayers.forEach((L, i) => {
    layerXs.set(L, EGO_PAD_X + i * EGO_COL_GAP);
  });
  const width = EGO_PAD_X * 2 + Math.max(0, sortedLayers.length - 1) * EGO_COL_GAP + 120;

  const laid: EgoLaidOutNode[] = [];
  for (const L of sortedLayers) {
    const col = buckets.get(L)!;
    const x = layerXs.get(L) ?? EGO_PAD_X;
    const n = col.length;
    const stackH = (n - 1) * EGO_ROW_PITCH;
    const y0 = centerY - stackH / 2;
    col.forEach((node, i) => {
      laid.push({
        id: node.id,
        name: node.name,
        class: (node.class || "candidate").toLowerCase(),
        path: node.path,
        line: node.line,
        kind: node.kind,
        layer: L,
        x,
        y: y0 + i * EGO_ROW_PITCH,
      });
    });
  }

  const laidIds = new Set(laid.map((n) => n.id));
  const outEdges: LayeredDagLaidOutEdge[] = [];
  const seen = new Set<string>();
  for (const e of edges) {
    if (!laidIds.has(e.from) || !laidIds.has(e.to)) continue;
    const k = `${e.from}\0${e.to}\0${e.kind ?? ""}`;
    if (seen.has(k)) continue;
    seen.add(k);
    outEdges.push({
      from: e.from,
      to: e.to,
      class: (e.class || "candidate").toLowerCase(),
      kind: e.kind || "import",
    });
  }
  outEdges.sort(
    (a, b) =>
      a.kind.localeCompare(b.kind) ||
      a.from.localeCompare(b.from) ||
      a.to.localeCompare(b.to),
  );
  laid.sort((a, b) => a.layer - b.layer || a.y - b.y || a.id.localeCompare(b.id));

  return { nodes: laid, edges: outEdges, truncated, width, height };
}
