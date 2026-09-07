// `lib/boardLayout.ts` — the board's geometry, and its determinism (V74-L2).
//
// Two properties, and both are the reason D10 says layout has ONE engine:
// identical input ⇒ identical output, and an AUTHORED pin is never overridden
// by a derived position.
import { describe, expect, it } from "vitest";
import type { BoardEdge, BoardNode } from "../api/types";
import { BOARD_H_STEP, BOARD_V_STEP, cameraFor, layoutBoard } from "./boardLayout";
import { EGO_PAD_X, EGO_PAD_Y } from "./egoGraph";
import { CANVAS_CARD_H, CANVAS_CARD_W } from "./canvasPlacement";

function n(id: string): BoardNode {
  return { id, kind: "note", state: "inert", reason: "no-reference", address: id };
}
function e(from: string, to: string, kind = "then"): BoardEdge {
  return { from, to, kind, provenance: "authored" };
}
function run(
  nodes: BoardNode[],
  edges: BoardEdge[] = [],
  steps: string[] = [],
  pins: Record<string, { x: number; y: number }> = {},
) {
  return layoutBoard({ nodes, edges, steps, pins, nodeCap: 200 });
}

describe("layoutBoard", () => {
  it("a chain reads left to right, one card per column", () => {
    const l = run([n("a"), n("b"), n("c")], [e("a", "b"), e("b", "c")]);
    expect(l.byId.get("a")!.x).toBe(EGO_PAD_X);
    expect(l.byId.get("b")!.x).toBe(EGO_PAD_X + BOARD_H_STEP);
    expect(l.byId.get("c")!.x).toBe(EGO_PAD_X + 2 * BOARD_H_STEP);
    // `from` is left of `to`: a board edge reads as flow, which is the OPPOSITE
    // of the engine's "depends on" convention, so the pair is flipped once, in
    // `layoutBoard`, and never re-flipped by a renderer.
    expect(l.byId.get("a")!.layer).toBe(0);
    expect(l.byId.get("c")!.layer).toBe(2);
  });

  it("siblings stack in STEP order first, then authored order", () => {
    const l = run(
      [n("root"), n("x"), n("y"), n("z")],
      [e("root", "x"), e("root", "y"), e("root", "z")],
      ["z", "y"],
    );
    expect(l.byId.get("z")!.y).toBe(EGO_PAD_Y);
    expect(l.byId.get("y")!.y).toBe(EGO_PAD_Y + BOARD_V_STEP);
    expect(l.byId.get("x")!.y).toBe(EGO_PAD_Y + 2 * BOARD_V_STEP);
    for (const id of ["x", "y", "z"]) {
      expect(l.byId.get(id)!.x).toBe(EGO_PAD_X + BOARD_H_STEP);
    }
  });

  it("a PIN wins exactly, and the pinned card is not flowed", () => {
    const l = run([n("a"), n("b")], [e("a", "b")], [], { b: { x: -7.5, y: 12.25 } });
    const b = l.byId.get("b")!;
    expect([b.x, b.y]).toEqual([-7.5, 12.25]);
    expect(b.pinned).toBe(true);
    // A pinned card has no COLUMN: it was not flowed, and reporting one would
    // dress a derived fact as an authored one.
    expect(b.layer).toBeNull();
    // …and the row it vacated is not held open for it.
    expect(l.byId.get("a")!.y).toBe(EGO_PAD_Y);
  });

  it("a pin for a node that is not on the board places nothing", () => {
    const l = run([n("a")], [], [], { ghost: { x: 1, y: 1 } });
    expect(l.byId.has("ghost")).toBe(false);
    expect(l.nodes).toHaveLength(1);
  });

  it("is coordinate-free otherwise — every unpinned position is a multiple of the pitch", () => {
    const l = run([n("a"), n("b"), n("c"), n("d")], [e("a", "b"), e("a", "c"), e("c", "d")]);
    for (const p of l.nodes) {
      expect((p.x - EGO_PAD_X) % BOARD_H_STEP, p.id).toBe(0);
      expect((p.y - EGO_PAD_Y) % BOARD_V_STEP, p.id).toBe(0);
      expect([p.w, p.h]).toEqual([CANVAS_CARD_W, CANVAS_CARD_H]);
    }
  });

  it("is byte-identical across runs", () => {
    const nodes = [n("a"), n("b"), n("c"), n("d")];
    const edges = [e("a", "b"), e("a", "c"), e("c", "d")];
    const one = run(nodes, edges);
    const two = run(nodes, edges);
    expect(JSON.stringify(one.nodes)).toBe(JSON.stringify(two.nodes));
    expect(JSON.stringify(one.edges)).toBe(JSON.stringify(two.edges));
  });

  it("a cycle terminates and still places every node", () => {
    const l = run([n("a"), n("b"), n("c")], [e("a", "b"), e("b", "c"), e("c", "a")]);
    expect(l.nodes).toHaveLength(3);
    for (const p of l.nodes) {
      expect(Number.isFinite(p.x) && Number.isFinite(p.y), p.id).toBe(true);
    }
  });

  it("an isolated node lands in layer zero rather than being dropped", () => {
    const l = run([n("lonely")]);
    expect(l.byId.get("lonely")).toMatchObject({ x: EGO_PAD_X, y: EGO_PAD_Y, layer: 0 });
  });

  it("an edge whose endpoint is missing draws nothing — and never a line to (0,0)", () => {
    const l = run([n("a")], [e("a", "ghost")]);
    expect(l.edges).toHaveLength(0);
  });

  it("edges connect card CENTRES, in board coordinates", () => {
    const l = run([n("a"), n("b")], [e("a", "b")]);
    const [edge] = l.edges;
    const a = l.byId.get("a")!;
    expect(edge.x1).toBe(a.x + CANVAS_CARD_W / 2);
    expect(edge.y1).toBe(a.y + CANVAS_CARD_H / 2);
  });

  it("the engine's cap is SURFACED, and a capped node still gets a position", () => {
    const many = Array.from({ length: 12 }, (_, i) => n(`n${i}`));
    const l = layoutBoard({ nodes: many, edges: [], steps: [], pins: {}, nodeCap: 5 });
    expect(l.truncated).toBe(7);
    // Every node is still PLACED — a card with no position would be the silent
    // disappearance this whole surface exists to prevent.
    expect(l.nodes).toHaveLength(12);
  });
});

describe("cameraFor", () => {
  it("centres the card in the viewport", () => {
    const l = run([n("a"), n("b")], [e("a", "b")]);
    const cam = cameraFor(l.byId.get("b"), 1000, 600);
    const b = l.byId.get("b")!;
    expect(cam.x + (b.x + b.w / 2)).toBe(500);
    expect(cam.y + (b.y + b.h / 2)).toBe(300);
  });

  it("a missing node parks the camera at the origin rather than at NaN", () => {
    expect(cameraFor(undefined, 800, 400)).toEqual({ x: 0, y: 0, zoom: 1 });
  });

  it("scales the offset with the zoom", () => {
    const l = run([n("a")]);
    const cam = cameraFor(l.byId.get("a"), 1000, 600, 2);
    const a = l.byId.get("a")!;
    expect(cam.x + (a.x + a.w / 2) * 2).toBe(500);
    expect(cam.zoom).toBe(2);
  });
});
