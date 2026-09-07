// `kbc-canvas/1` — the board's GEOMETRY, and its only home (V74-L2, D10).
//
// D10 is explicit: *"Layout stays in TypeScript, one engine (the golden-tested
// layered DAG already exists): boards are stored coordinate-free plus pins, so
// the LLM never emits coordinates."* Two consequences this module enforces:
//
// **The engine is `egoGraph.ts`'s `layoutLayeredDag`, not a second one.** This
// file computes NO topology of its own — no layering, no cycle break, no
// within-layer ordering. It hands the engine the node set and the edges, and
// then does exactly two things the engine has no notion of: it scales the
// engine's grid to CARD geometry, and it honours PINS. If you find yourself
// adding a traversal here, the honest fix is to widen the engine (and its
// golden) instead.
//
// **A pin wins, and a pinned card is not flowed.** `boards::layout::place`
// (the server's export-only layout) makes the same two choices for the same
// reason: an authored position is never overridden by a derived one, and
// leaving a pinned card in the flow would push its neighbours around a hole it
// no longer occupies. The server's module doc says the two are the same
// FAMILY, deliberately not a port; where they must agree pixel-for-pixel, the
// answer is for this side to send its positions as pins.
//
// Determinism is the whole contract: identical input ⇒ identical output, no
// clock, no randomness, no measurement of the DOM. `boardLayout.test.ts` is
// that assertion.

import type { BoardEdge, BoardNode, BoardPin } from "../api/types";
import { CANVAS_CARD_H, CANVAS_CARD_W } from "./canvasPlacement";
import { EGO_COL_GAP, EGO_PAD_X, EGO_PAD_Y, EGO_ROW_PITCH, layoutLayeredDag } from "./egoGraph";

/// Card pitch: the engine's own gaps, applied to a card rather than to a
/// 112×28 ego node. `boards::layout`'s `CARD_W + COL_GAP` / `CARD_H + ROW_GAP`,
/// the same two numbers, so a JSON Canvas export and this surface land on one
/// grid.
export const BOARD_H_STEP = CANVAS_CARD_W + EGO_COL_GAP;
export const BOARD_V_STEP = CANVAS_CARD_H + EGO_ROW_PITCH;

export interface PlacedNode {
  id: string;
  x: number;
  y: number;
  w: number;
  h: number;
  /// The engine's layer. `null` for a PINNED node: it was not flowed, so it
  /// has no column, and reporting one would be a derived fact dressed as an
  /// authored one.
  layer: number | null;
  pinned: boolean;
}

export interface PlacedEdge {
  from: string;
  to: string;
  /// Centre-to-centre, in board coordinates. The renderer draws the line; it
  /// does not decide where it goes.
  x1: number;
  y1: number;
  x2: number;
  y2: number;
}

export interface BoardLayout {
  nodes: PlacedNode[];
  byId: Map<string, PlacedNode>;
  edges: PlacedEdge[];
  width: number;
  height: number;
  /// Nodes the engine's cap dropped. Surfaced, never silent.
  truncated: number;
}

/// The rank key that decides ROW ORDER inside a layer.
///
/// The engine sorts a layer by `name` then `id`, so the order is expressed AS
/// a name rather than applied afterwards — which keeps this module free of a
/// second sort that could disagree with the engine's own. Step order first (a
/// walkthrough's reading order is the best row order there is), then the
/// author's own node order, exactly as `boards::layout::place` ranks them.
function rankKey(index: number, stepRank: number | undefined): string {
  const bucket = stepRank === undefined ? "9" : "0";
  const within = String(stepRank ?? index).padStart(6, "0");
  return `${bucket}:${within}:${String(index).padStart(6, "0")}`;
}

export interface LayoutInput {
  nodes: BoardNode[];
  edges: BoardEdge[];
  /// Node ids in walkthrough order.
  steps: string[];
  pins: Record<string, BoardPin>;
  /// The daemon's own `honesty.budget.max_nodes`. Passed in rather than
  /// hardcoded so the cap on screen is the cap the daemon enforced.
  nodeCap: number;
}

export function layoutBoard(input: LayoutInput): BoardLayout {
  const stepRank = new Map<string, number>();
  input.steps.forEach((id, i) => {
    if (!stepRank.has(id)) stepRank.set(id, i);
  });

  const laid = layoutLayeredDag({
    nodes: input.nodes.map((n, i) => ({
      id: n.id,
      name: rankKey(i, stepRank.get(n.id)),
    })),
    // The engine reads an edge as "`from` DEPENDS ON `to`", so it puts `to`
    // to the LEFT. A board edge reads the other way round (`a calls b` should
    // draw a left of b), so the pair is flipped here rather than the engine's
    // own convention being bent for one caller.
    edges: input.edges.map((e) => ({ from: e.to, to: e.from, kind: e.kind })),
    nodeCap: input.nodeCap,
  });

  const layerOf = new Map<string, number>();
  const engineOrder = new Map<string, number>();
  laid.nodes.forEach((n, i) => {
    layerOf.set(n.id, n.layer);
    engineOrder.set(n.id, i);
  });

  // Rows are assigned over the UNPINNED nodes only, in the engine's own
  // within-layer order — the server's `by_layer` skip, mirrored.
  const rows = new Map<string, number>();
  const nextRow = new Map<number, number>();
  const flowed = input.nodes
    .filter((n) => !input.pins[n.id] && engineOrder.has(n.id))
    .sort((a, b) => (engineOrder.get(a.id) ?? 0) - (engineOrder.get(b.id) ?? 0));
  for (const n of flowed) {
    const layer = layerOf.get(n.id) ?? 0;
    const row = nextRow.get(layer) ?? 0;
    rows.set(n.id, row);
    nextRow.set(layer, row + 1);
  }

  const placed: PlacedNode[] = [];
  for (const n of input.nodes) {
    const pin = input.pins[n.id];
    if (pin) {
      placed.push({
        id: n.id,
        x: pin.x,
        y: pin.y,
        w: CANVAS_CARD_W,
        h: CANVAS_CARD_H,
        layer: null,
        pinned: true,
      });
      continue;
    }
    // A node the engine's cap dropped still gets a position: the board says
    // `truncated` out loud, and a card with no place would be the silent
    // disappearance this whole surface exists to prevent.
    const layer = layerOf.get(n.id) ?? 0;
    const row = rows.get(n.id) ?? 0;
    placed.push({
      id: n.id,
      x: EGO_PAD_X + layer * BOARD_H_STEP,
      y: EGO_PAD_Y + row * BOARD_V_STEP,
      w: CANVAS_CARD_W,
      h: CANVAS_CARD_H,
      layer,
      pinned: false,
    });
  }

  const byId = new Map(placed.map((p) => [p.id, p]));
  const edges: PlacedEdge[] = [];
  for (const e of input.edges) {
    const a = byId.get(e.from);
    const b = byId.get(e.to);
    if (!a || !b) continue;
    edges.push({
      from: e.from,
      to: e.to,
      x1: a.x + a.w / 2,
      y1: a.y + a.h / 2,
      x2: b.x + b.w / 2,
      y2: b.y + b.h / 2,
    });
  }

  const width = placed.reduce((m, p) => Math.max(m, p.x + p.w), 0) + EGO_PAD_X;
  const height = placed.reduce((m, p) => Math.max(m, p.y + p.h), 0) + EGO_PAD_Y;
  return { nodes: placed, byId, edges, width, height, truncated: laid.truncated };
}

/// The camera for ONE walkthrough step: the transform that centres a card in a
/// viewport of `vw × vh`. Pure arithmetic over the layout — a step's camera is
/// derived from where the card IS, never stored, so reordering the steps or
/// re-laying the board can never leave a camera pointing at nothing.
export interface Camera {
  x: number;
  y: number;
  zoom: number;
}

export function cameraFor(
  node: PlacedNode | undefined,
  vw: number,
  vh: number,
  zoom = 1,
): Camera {
  if (!node) return { x: 0, y: 0, zoom };
  return {
    x: vw / 2 - (node.x + node.w / 2) * zoom,
    y: vh / 2 - (node.y + node.h / 2) * zoom,
    zoom,
  };
}
