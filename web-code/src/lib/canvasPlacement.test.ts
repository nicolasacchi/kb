import { describe, expect, it } from "vitest";
import {
  CANVAS_CARD_H,
  CANVAS_CARD_W,
  CANVAS_H_STEP,
  CANVAS_V_STEP,
  edgesFromHierarchy,
  findFreeSlot,
  placeBeside,
  preferredBeside,
} from "./canvasPlacement";
import type { CanvasFragment } from "./canvasPayload";
import { fragmentKey } from "./canvasPayload";
import { EGO_COL_GAP, EGO_ROW_PITCH } from "./egoGraph";

describe("preferredBeside / findFreeSlot / placeBeside", () => {
  it("reuses layered-dag geometry constants for spacing", () => {
    expect(CANVAS_H_STEP).toBe(EGO_COL_GAP + CANVAS_CARD_W);
    expect(CANVAS_V_STEP).toBeGreaterThanOrEqual(EGO_ROW_PITCH * 4);
  });

  it("places callees to the right and callers to the left of source", () => {
    const src = { x: 100, y: 200, w: CANVAS_CARD_W };
    expect(preferredBeside(src, "callee")).toEqual({
      x: 100 + CANVAS_CARD_W + EGO_COL_GAP,
      y: 200,
    });
    expect(preferredBeside(src, "caller")).toEqual({
      x: 100 - CANVAS_H_STEP,
      y: 200,
    });
  });

  it("returns preferred when free", () => {
    expect(findFreeSlot([], { x: 40, y: 40 })).toEqual({ x: 40, y: 40 });
  });

  it("scans to a free slot when preferred is occupied (deterministic)", () => {
    const existing: CanvasFragment[] = [
      { path: "a.rs", symbol: "a", line: 1, x: 40, y: 40 },
    ];
    const first = findFreeSlot(existing, { x: 40, y: 40 });
    // Must not land on the occupied origin.
    expect(first).not.toEqual({ x: 40, y: 40 });
    // Same inputs → same output.
    expect(findFreeSlot(existing, { x: 40, y: 40 })).toEqual(first);

    // The free slot itself must not overlap the existing card rect.
    const overlaps =
      Math.abs(first.x - 40) < CANVAS_CARD_W + 8 && Math.abs(first.y - 40) < CANVAS_CARD_H + 8;
    expect(overlaps).toBe(false);
  });

  it("placeBeside without source starts near origin free slot", () => {
    const p = placeBeside([], null, "free");
    expect(p).toEqual({ x: 40, y: 40 });
  });

  it("placeBeside with source prefers the edge direction", () => {
    const src: CanvasFragment = { path: "a.rs", symbol: "a", line: 1, x: 100, y: 50 };
    const p = placeBeside([src], src, "callee");
    expect(p.x).toBeGreaterThan(src.x);
    expect(p.y).toBe(50);
  });
});

describe("edgesFromHierarchy", () => {
  it("links call edges between on-canvas fragments", () => {
    const a = { path: "caller.rs", symbol: "caller_fn", line: 5 };
    const b = { path: "lib.rs", symbol: "target", line: 10 };
    const ka = fragmentKey(a);
    const kb = fragmentKey(b);
    const keys = new Map([
      [ka, a],
      [kb, b],
    ]);
    const callees = new Map([
      [ka, [{ path: "lib.rs", name: "target", line: 10, class: "exact" }]],
    ]);
    const callers = new Map<string, Array<{ path: string; name: string; line?: number; class?: string }>>();
    const edges = edgesFromHierarchy(keys, callees, callers);
    expect(edges).toEqual([
      { fromKey: ka, toKey: kb, kind: "call", class: "exact" },
    ]);
  });

  it("dedupes caller-side and callee-side discovery of the same edge", () => {
    const a = { path: "a.rs", symbol: "a", line: 1 };
    const b = { path: "b.rs", symbol: "b", line: 2 };
    const ka = fragmentKey(a);
    const kb = fragmentKey(b);
    const keys = new Map([
      [ka, a],
      [kb, b],
    ]);
    const callees = new Map([[ka, [{ path: "b.rs", name: "b", class: "likely" }]]]);
    const callers = new Map([[kb, [{ path: "a.rs", name: "a", class: "exact" }]]]);
    const edges = edgesFromHierarchy(keys, callees, callers);
    expect(edges).toHaveLength(1);
    // First discovery wins the class (callee-side here).
    expect(edges[0]!.class).toBe("likely");
  });
});
