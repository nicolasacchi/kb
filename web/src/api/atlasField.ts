// W3.F-c — the OPERATOR FIELD wire: the client half of the dual-field
// atlas.
//
// Two layouts of the same corpus are overlaid: the MACHINE field (the
// atlas's own embedding-derived coordinates) and the OPERATOR field — a
// JSON Canvas 1.0 sidecar (`<source_root>/atlas/operator.canvas`) the
// daemon stores VERBATIM (`crates/kb-server/src/routes/atlas_field.rs`).
// Where the two disagree is the point: it is the visible gap between the
// operator's mental map and the model's.
//
// Three rules this module exists to keep:
//
//  1. ONE JSON Canvas codec. `lib/canvas.ts` (already shipped, already
//     used by boards) parses/serializes/patches the document; nothing here
//     re-implements a second parser. The only field-specific knowledge
//     added on top is the unit↔grid projection and the "one `file` node
//     per artifact, first wins" join rule, both mirrored from
//     `kb_core::atlas_field`.
//  2. The ALIGNMENT IS THE SERVER'S. `GET .../atlas/field/disagreement`
//     returns each artifact's operator position ALREADY Procrustes-aligned
//     into the machine frame, plus the raw placement and the distance. A
//     renderer must never re-fit that (the same rule the time-lapse frames
//     follow — the daemon aligns so `kb atlas field diff` and the SPA agree
//     byte for byte). [`recoverFieldTransform`] below is NOT a re-fit: it
//     algebraically RECOVERS the server's own map from the server's own
//     (raw → aligned) pairs, so island rectangles — which the wire carries
//     no aligned coordinates for — land in the same frame the ghost dots do.
//  3. Machine coordinates are READ-ONLY. Nothing in this module (or its
//     callers) writes an atlas coordinate; a drag writes the SIDECAR only.

import { currentDaemonBase } from "./base";
import {
  addNode,
  moveNode,
  parseCanvas,
  serializeCanvas,
  DEFAULT_CARD_HEIGHT,
  DEFAULT_CARD_WIDTH,
  type CanvasDoc,
  type CanvasNode,
} from "../lib/canvas";
import type { AtlasFieldDisagreementOut } from "./generated/AtlasFieldDisagreementOut";
import type { AtlasFieldDisagreementResponse } from "./generated/AtlasFieldDisagreementResponse";

export type { AtlasFieldDisagreementOut, AtlasFieldDisagreementResponse };

// --- wire ---------------------------------------------------------------

function fieldPath(kb: string): string {
  return `${currentDaemonBase()}/api/kb/${encodeURIComponent(kb)}/atlas/field`;
}

async function failed(r: Response, prefix: string): Promise<never> {
  const ct = r.headers.get("content-type") ?? "";
  let detail: string | undefined;
  if (ct.includes("application/problem+json")) {
    try {
      const problem = (await r.json()) as { title?: string; detail?: string };
      detail = `${problem.title ?? "error"}: ${problem.detail ?? ""}`;
    } catch {
      /* torn/empty problem body — fall through to the status line */
    }
  }
  throw new Error(`${prefix}: ${detail ?? `${r.status} ${r.statusText}`}`);
}

/** `GET /api/kb/{kb}/atlas/field` — the raw JSON Canvas sidecar. A kb that
 * has never been hand-placed answers with the empty document (the daemon
 * never 404s here), so this resolves to `{nodes:[],edges:[]}` rather than
 * throwing. Parsed through the SHIPPED codec so unknown fields survive. */
export async function fetchAtlasField(
  kb: string,
  signal?: AbortSignal,
): Promise<CanvasDoc> {
  const r = await fetch(fieldPath(kb), {
    headers: { Accept: "application/json" },
    signal,
  });
  if (!r.ok) await failed(r, "load operator field failed");
  return parseCanvas(await r.json());
}

/** `PUT /api/kb/{kb}/atlas/field` — replace the sidecar wholesale (the
 * daemon has no partial-update route by design: "store what parses",
 * verbatim). Body is the serialized document, not a JSON envelope. */
export async function putAtlasField(
  kb: string,
  doc: CanvasDoc,
): Promise<CanvasDoc> {
  const r = await fetch(fieldPath(kb), {
    method: "PUT",
    headers: { "Content-Type": "application/json", Accept: "application/json" },
    body: serializeCanvas(doc),
  });
  if (!r.ok) await failed(r, "save operator field failed");
  return parseCanvas(await r.json());
}

/** `GET /api/kb/{kb}/atlas/field/disagreement` — per-artifact displacement
 * between the machine layout and the operator's field, LARGEST FIRST,
 * Procrustes-aligned server-side. Join key is the source-relative path. */
export async function fetchAtlasFieldDisagreement(
  kb: string,
  signal?: AbortSignal,
): Promise<AtlasFieldDisagreementResponse> {
  const r = await fetch(`${fieldPath(kb)}/disagreement`, {
    headers: { Accept: "application/json" },
    signal,
  });
  if (!r.ok) await failed(r, "load field disagreement failed");
  return (await r.json()) as AtlasFieldDisagreementResponse;
}

// --- unit ↔ grid --------------------------------------------------------
//
// JSON Canvas positions are an integer pixel grid; the atlas is `[0,1]`
// unit space. `kb_core::atlas_field` bridges them through a fixed
// `0..=1000` grid with a PINNED rounding rule (round half up, NaN → 0,
// clamp to the field). These two functions mirror that rule exactly — a
// coordinate written here must read back identically on the daemon.

/** The integer grid the `[0,1]` unit square maps onto — mirrors
 * `kb_core::atlas_field::FIELD_GRID`. */
export const FIELD_GRID = 1000;

/** Grid → unit. Out-of-field values clamp to the nearest edge. */
export function unitFromGrid(g: number): number {
  if (!Number.isFinite(g)) return 0;
  return Math.min(Math.max(g, 0), FIELD_GRID) / FIELD_GRID;
}

/** Unit → grid: clamp to `[0,1]`, then round HALF UP (`floor(u*1000+0.5)`).
 * `NaN` goes to the field origin, matching the Rust side's explicit
 * NaN-before-clamp branch. */
export function gridFromUnit(u: number): number {
  if (Number.isNaN(u)) return 0;
  const c = Math.min(Math.max(u, 0), 1);
  return Math.min(Math.max(Math.floor(c * FIELD_GRID + 0.5), 0), FIELD_GRID);
}

// --- reading the field --------------------------------------------------

/** One operator placement, in unit coordinates. `file` is the artifact's
 * SOURCE-RELATIVE PATH (the join key the daemon uses — not the artifact
 * id), matching what `canvas.ts` writes into a `"file"` node. */
export type FieldPlacement = {
  nodeId: string;
  file: string;
  x: number;
  y: number;
};

/** The placed artifacts, keyed by source-relative path. FIRST occurrence
 * wins, exactly like `kb_core::atlas_field`'s own `or_insert` join —
 * placing the same artifact twice is legal JSON Canvas, and the first
 * placement is that artifact's position on both sides of the wire. */
export function fieldPlacements(doc: CanvasDoc): Map<string, FieldPlacement> {
  const out = new Map<string, FieldPlacement>();
  for (const n of doc.nodes) {
    if (n.type !== "file" || typeof n.file !== "string") continue;
    if (typeof n.x !== "number" || typeof n.y !== "number") continue;
    if (out.has(n.file)) continue;
    out.set(n.file, {
      nodeId: n.id,
      file: n.file,
      x: unitFromGrid(n.x),
      y: unitFromGrid(n.y),
    });
  }
  return out;
}

/** An operator-named ISLAND — a JSON Canvas `type:"group"` node, in unit
 * coordinates (`w`/`h` are unit-space extents, i.e. grid extents / 1000).
 *
 * The label is an OPERATOR-TYPED string, rendered verbatim. kb never
 * generates one: no in-daemon LLM, and no client-side LLM either — an
 * island is named by the hand that drew it or it has no name. */
export type FieldIsland = {
  nodeId: string;
  label: string | null;
  x: number;
  y: number;
  w: number;
  h: number;
  color: string | null;
};

/** The operator-named regions, in document order. */
export function fieldIslands(doc: CanvasDoc): FieldIsland[] {
  const out: FieldIsland[] = [];
  for (const n of doc.nodes) {
    if (n.type !== "group") continue;
    if (typeof n.x !== "number" || typeof n.y !== "number") continue;
    const w = typeof n.width === "number" ? n.width : DEFAULT_CARD_WIDTH;
    const h = typeof n.height === "number" ? n.height : DEFAULT_CARD_HEIGHT;
    out.push({
      nodeId: n.id,
      label: typeof n.label === "string" && n.label.length > 0 ? n.label : null,
      x: unitFromGrid(n.x),
      y: unitFromGrid(n.y),
      // Extents are DIFFERENCES, not positions: divide by the grid without
      // the position clamp (a clamp would silently shrink a wide island).
      w: Number.isFinite(w) ? w / FIELD_GRID : 0,
      h: Number.isFinite(h) ? h / FIELD_GRID : 0,
      color: typeof n.color === "string" ? n.color : null,
    });
  }
  return out;
}

/** Next free `kbf<N>` node id for a field-authored placement. Deterministic
 * (max existing suffix + 1) rather than random/time-seeded, so a test can
 * assert the document a drag produces. */
export function nextFieldNodeId(doc: CanvasDoc): string {
  let max = 0;
  for (const n of doc.nodes) {
    const m = /^kbf(\d+)$/.test(n.id) ? Number(n.id.slice(3)) : 0;
    if (m > max) max = m;
  }
  return `kbf${max + 1}`;
}

/** Set one artifact's operator position, in UNIT coordinates: move the
 * existing `"file"` node for `file` if there is one, else append a fresh
 * one at the standard card size. Everything else on the document — other
 * nodes, edges, unknown top-level keys — round-trips untouched (that is
 * `canvas.ts`'s whole contract, and the sidecar is shared with Obsidian).
 *
 * Writes ONLY the sidecar. The machine layout is read-only: no atlas
 * coordinate, lance row or index generation is touched by a placement
 * (the `.canvas` extension is claimed by no `ExtensionMap`, so the
 * indexer never even sees the file). */
export function setPlacement(
  doc: CanvasDoc,
  file: string,
  ux: number,
  uy: number,
): CanvasDoc {
  const gx = gridFromUnit(ux);
  const gy = gridFromUnit(uy);
  const existing = fieldPlacements(doc).get(file);
  if (existing) return moveNode(doc, existing.nodeId, gx, gy);
  const node: CanvasNode = {
    id: nextFieldNodeId(doc),
    type: "file",
    x: gx,
    y: gy,
    width: DEFAULT_CARD_WIDTH,
    height: DEFAULT_CARD_HEIGHT,
    file,
  };
  return addNode(doc, node);
}

// --- recovering the server's alignment ----------------------------------

/** The affine map the daemon's Procrustes fit applies to the operator
 * field: `(x,y) ↦ (a·x + b·y + tx, c·x + d·y + ty)`. */
export type FieldTransform = {
  a: number;
  b: number;
  c: number;
  d: number;
  tx: number;
  ty: number;
};

export const IDENTITY_FIELD_TRANSFORM: FieldTransform = {
  a: 1,
  b: 0,
  c: 0,
  d: 1,
  tx: 0,
  ty: 0,
};

/** Apply a recovered transform to a raw (as-placed) unit point. */
export function applyFieldTransform(
  t: FieldTransform,
  x: number,
  y: number,
): { x: number; y: number } {
  return { x: t.a * x + t.b * y + t.tx, y: t.c * x + t.d * y + t.ty };
}

/** Recover the daemon's alignment from its OWN output.
 *
 * Every disagreement row carries the same point twice: `operator_raw_*`
 * (as placed) and `operator_*` (after the server's Procrustes fit). The map
 * between them is a similarity (rotate · uniform-scale · translate, possibly
 * REFLECTED — `procrustes::Transform::reflect`), hence affine, hence
 * determined exactly by three non-collinear pairs. Solving for it is
 * ALGEBRA over numbers the server already computed, not a second fit: feed
 * it the server's rows and you get the server's transform back, to
 * floating-point. That matters because islands (`type:"group"` nodes) exist
 * only in the raw sidecar — the wire carries no aligned coordinates for
 * them — and drawing them in a frame the ghost dots don't share would be a
 * lie about where the operator drew them.
 *
 * Deterministic anchor choice (input order never matters, because the
 * server's row order is itself deterministic — distance desc, id asc):
 * take the first row, then the row farthest from it, then the row farthest
 * from the line through those two; ties break on the smaller id.
 *
 * `null` when the rows can't determine a transform (fewer than three, or
 * every placement collinear/coincident — a degenerate case the caller must
 * treat as "can't place the rest of the field", not as identity, since an
 * identity guess would silently draw islands in the wrong frame). */
export function recoverFieldTransform(
  rows: readonly AtlasFieldDisagreementOut[],
): FieldTransform | null {
  if (rows.length < 3) return null;
  const p0 = rows[0];
  let p1: AtlasFieldDisagreementOut | null = null;
  let best = 0;
  for (const r of rows) {
    const dx = r.operator_raw_x - p0.operator_raw_x;
    const dy = r.operator_raw_y - p0.operator_raw_y;
    const d2 = dx * dx + dy * dy;
    if (d2 > best || (p1 !== null && d2 === best && r.id < p1.id)) {
      best = d2;
      p1 = r;
    }
  }
  if (!p1 || best <= 0) return null;
  const u1x = p1.operator_raw_x - p0.operator_raw_x;
  const u1y = p1.operator_raw_y - p0.operator_raw_y;
  let p2: AtlasFieldDisagreementOut | null = null;
  let bestCross = 0;
  for (const r of rows) {
    const dx = r.operator_raw_x - p0.operator_raw_x;
    const dy = r.operator_raw_y - p0.operator_raw_y;
    const cross = Math.abs(u1x * dy - u1y * dx);
    if (
      cross > bestCross ||
      (p2 !== null && cross === bestCross && r.id < p2.id)
    ) {
      bestCross = cross;
      p2 = r;
    }
  }
  if (!p2 || bestCross <= 0) return null;
  const u2x = p2.operator_raw_x - p0.operator_raw_x;
  const u2y = p2.operator_raw_y - p0.operator_raw_y;
  const det = u1x * u2y - u1y * u2x;
  if (!Number.isFinite(det) || det === 0) return null;
  const v1x = p1.operator_x - p0.operator_x;
  const v1y = p1.operator_y - p0.operator_y;
  const v2x = p2.operator_x - p0.operator_x;
  const v2y = p2.operator_y - p0.operator_y;
  // [v1 v2] = A · [u1 u2]  ⇒  A = [v1 v2] · [u1 u2]⁻¹
  const a = (v1x * u2y - v2x * u1y) / det;
  const b = (v2x * u1x - v1x * u2x) / det;
  const c = (v1y * u2y - v2y * u1y) / det;
  const d = (v2y * u1x - v1y * u2x) / det;
  const tx = p0.operator_x - (a * p0.operator_raw_x + b * p0.operator_raw_y);
  const ty = p0.operator_y - (c * p0.operator_raw_x + d * p0.operator_raw_y);
  if (![a, b, c, d, tx, ty].every((n) => Number.isFinite(n))) return null;
  return { a, b, c, d, tx, ty };
}

// --- the disagreement heat ramp -----------------------------------------

/** Normalised displacement in `[0,1]`: this row's distance over the
 * largest in the set. `0` for a degenerate/empty set (nothing to compare
 * against), never `NaN`. The MAGNITUDES ARE THE SERVER'S — this only picks
 * where they sit on the ramp. */
export function heatT(distance: number, max: number): number {
  if (!Number.isFinite(distance) || !Number.isFinite(max) || max <= 0) return 0;
  return Math.min(Math.max(distance / max, 0), 1);
}

/** Cool → hot ramp for the disagreement heat mode: agreement is a calm
 * blue, the largest displacement is a warm red. Interpolated in plain sRGB
 * (the atlas palette is sRGB hex too) and returned as `rgb(...)` so the
 * canvas can use it directly. */
export function heatColor(t: number): string {
  const k = Number.isFinite(t) ? Math.min(Math.max(t, 0), 1) : 0;
  // #5b8def (the atlas's own blue) → #ffb74d (amber) → #e2564a (red).
  const stops: [number, number, number][] = [
    [0x5b, 0x8d, 0xef],
    [0xff, 0xb7, 0x4d],
    [0xe2, 0x56, 0x4a],
  ];
  const seg = k >= 1 ? 1 : Math.floor(k * 2);
  const local = k * 2 - seg;
  const from = stops[seg];
  const to = stops[seg + 1];
  const mix = (i: number) => Math.round(from[i] + (to[i] - from[i]) * local);
  return `rgb(${mix(0)}, ${mix(1)}, ${mix(2)})`;
}
