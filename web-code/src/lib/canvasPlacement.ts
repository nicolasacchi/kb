// V3.4-C2 — deterministic free-slot placement for canvas fragments.
// Reuses `layoutLayeredDag` geometry constants (EGO_COL_GAP / EGO_ROW_PITCH)
// for spacing so call/import neighbor cards land on the same visual grid
// as the review-map ego layout family. No physics, no randomness.

import { EGO_COL_GAP, EGO_ROW_PITCH } from "./egoGraph";
import type { CanvasFragment } from "./canvasPayload";

/** Default fragment card size (px) — layout slots sized to this. */
export const CANVAS_CARD_W = 320;
export const CANVAS_CARD_H = 200;

/** Horizontal step when placing a callee / caller beside a source card. */
export const CANVAS_H_STEP = EGO_COL_GAP + CANVAS_CARD_W; // 500
/** Vertical step for free-slot scan / stack within a column. */
export const CANVAS_V_STEP = Math.max(EGO_ROW_PITCH * 4, CANVAS_CARD_H + EGO_ROW_PITCH); // 248

export type EdgeDirection = "callee" | "caller" | "import" | "free";

export interface Rect {
  x: number;
  y: number;
  w: number;
  h: number;
}

function rectsOverlap(a: Rect, b: Rect, pad = 8): boolean {
  return !(
    a.x + a.w + pad <= b.x ||
    b.x + b.w + pad <= a.x ||
    a.y + a.h + pad <= b.y ||
    b.y + b.h + pad <= a.y
  );
}

function occupiedRects(fragments: CanvasFragment[], defaultW = CANVAS_CARD_W, defaultH = CANVAS_CARD_H): Rect[] {
  return fragments.map((f) => ({
    x: f.x,
    y: f.y,
    w: f.w && f.w > 0 ? f.w : defaultW,
    h: defaultH,
  }));
}

/**
 * Preferred anchor for a new card relative to a source card, given the
 * relation direction (callees right, callers left, imports further right).
 */
export function preferredBeside(
  source: { x: number; y: number; w?: number },
  direction: EdgeDirection,
): { x: number; y: number } {
  const sw = source.w && source.w > 0 ? source.w : CANVAS_CARD_W;
  switch (direction) {
    case "caller":
      return { x: source.x - CANVAS_H_STEP, y: source.y };
    case "callee":
    case "import":
      return { x: source.x + sw + EGO_COL_GAP, y: source.y };
    case "free":
    default:
      return { x: source.x + sw + EGO_COL_GAP, y: source.y };
  }
}

/**
 * Deterministic free-slot scan: try the preferred point, then spiral
 * outward on a grid stepped by CANVAS_H_STEP × CANVAS_V_STEP. First
 * non-overlapping slot wins. Pure + identical for identical inputs.
 */
export function findFreeSlot(
  existing: CanvasFragment[],
  preferred: { x: number; y: number },
  cardW = CANVAS_CARD_W,
  cardH = CANVAS_CARD_H,
  maxRadius = 24,
): { x: number; y: number } {
  const occupied = occupiedRects(existing, cardW, cardH);
  const candidate = (x: number, y: number): boolean => {
    const r: Rect = { x, y, w: cardW, h: cardH };
    return !occupied.some((o) => rectsOverlap(r, o));
  };

  if (candidate(preferred.x, preferred.y)) {
    return { x: preferred.x, y: preferred.y };
  }

  // Spiral: ring r, then (dx,dy) offsets in a fixed order (right, down, left, up).
  for (let r = 1; r <= maxRadius; r++) {
    for (let dy = -r; dy <= r; dy++) {
      for (let dx = -r; dx <= r; dx++) {
        if (Math.max(Math.abs(dx), Math.abs(dy)) !== r) continue;
        const x = preferred.x + dx * CANVAS_H_STEP;
        const y = preferred.y + dy * CANVAS_V_STEP;
        if (candidate(x, y)) return { x, y };
      }
    }
  }
  // Exhausted — stack below preferred with a vertical nudge (still deterministic).
  return {
    x: preferred.x,
    y: preferred.y + (maxRadius + 1) * CANVAS_V_STEP,
  };
}

/**
 * Place a new fragment beside `source` along `direction`, scanning for a
 * free slot. When `source` is absent (first card / add-from-search), place
 * at the origin free slot.
 */
export function placeBeside(
  existing: CanvasFragment[],
  source: CanvasFragment | null | undefined,
  direction: EdgeDirection = "free",
): { x: number; y: number } {
  if (!source) {
    return findFreeSlot(existing, { x: 40, y: 40 });
  }
  return findFreeSlot(existing, preferredBeside(source, direction));
}

/** Edge drawn between two cards that share a call/import relation. */
export interface CanvasEdge {
  fromKey: string;
  toKey: string;
  /** `"call"` | `"import"` — class shown in tooltip, never upgraded. */
  kind: string;
  class: string;
}

/**
 * Build edges from hierarchy caller/callee results already fetched for
 * fragments on the canvas. `fromKey` is the caller, `toKey` the callee
 * (arrow direction: call flows toward the callee). Import edges use the
 * same shape with `kind: "import"`.
 */
export function edgesFromHierarchy(
  fragmentKeys: Map<string, { path: string; symbol: string; line: number }>,
  /** For each fragment key: callee sites resolved to (path, name, line?, class). */
  calleesByKey: Map<string, Array<{ path: string; name: string; line?: number; class?: string }>>,
  /** For each fragment key: caller groups resolved to (path, name, line?, class). */
  callersByKey: Map<string, Array<{ path: string; name: string; line?: number; class?: string }>>,
): CanvasEdge[] {
  const byPathSymbol = new Map<string, string[]>();
  for (const [key, f] of fragmentKeys) {
    const k = `${f.path}\0${f.symbol}`;
    const arr = byPathSymbol.get(k) ?? [];
    arr.push(key);
    byPathSymbol.set(k, arr);
  }
  function matchKey(path: string, name: string, line?: number): string | null {
    const arr = byPathSymbol.get(`${path}\0${name}`);
    if (!arr || arr.length === 0) return null;
    if (arr.length === 1 || line === undefined) return arr[0]!;
    // Prefer the fragment whose pinned line is nearest.
    let best = arr[0]!;
    let bestDist = Infinity;
    for (const key of arr) {
      const f = fragmentKeys.get(key)!;
      const d = Math.abs(f.line - line);
      if (d < bestDist) {
        best = key;
        bestDist = d;
      }
    }
    return best;
  }

  const out: CanvasEdge[] = [];
  const seen = new Set<string>();

  for (const [fromKey, callees] of calleesByKey) {
    for (const c of callees) {
      const toKey = matchKey(c.path, c.name, c.line);
      if (!toKey || toKey === fromKey) continue;
      const id = `${fromKey}->${toKey}:call`;
      if (seen.has(id)) continue;
      seen.add(id);
      out.push({
        fromKey,
        toKey,
        kind: "call",
        class: (c.class || "candidate").toLowerCase(),
      });
    }
  }

  for (const [toKey, callers] of callersByKey) {
    for (const c of callers) {
      const fromKey = matchKey(c.path, c.name, c.line);
      if (!fromKey || fromKey === toKey) continue;
      const id = `${fromKey}->${toKey}:call`;
      if (seen.has(id)) continue;
      seen.add(id);
      out.push({
        fromKey,
        toKey,
        kind: "call",
        class: (c.class || "candidate").toLowerCase(),
      });
    }
  }

  out.sort(
    (a, b) =>
      a.kind.localeCompare(b.kind) ||
      a.fromKey.localeCompare(b.fromKey) ||
      a.toKey.localeCompare(b.toKey),
  );
  return out;
}
