import { describe, expect, it } from "vitest";
import {
  applyFieldTransform,
  FIELD_GRID,
  fieldIslands,
  fieldPlacements,
  gridFromUnit,
  heatColor,
  heatT,
  nextFieldNodeId,
  recoverFieldTransform,
  setPlacement,
  unitFromGrid,
  type AtlasFieldDisagreementOut,
} from "./atlasField";
import { parseCanvas, type CanvasDoc } from "../lib/canvas";

// W3.F-c — the pure half of the operator-field client. The wire functions
// are plain fetch wrappers (covered by the e2e/live surface); everything
// tested here is the math the daemon ALSO implements, so a drift between
// the two shows up as a failure right here rather than as a coordinate
// that reads back a pixel off.

describe("unit ↔ grid", () => {
  it("round-trips every integer on the grid (mirrors the Rust pin)", () => {
    for (let g = 0; g <= FIELD_GRID; g += 1) {
      expect(gridFromUnit(unitFromGrid(g))).toBe(g);
    }
  });

  it("clamps out-of-field grid coordinates to the edges", () => {
    expect(unitFromGrid(-50)).toBe(0);
    expect(unitFromGrid(4000)).toBe(1);
  });

  it("rounds half UP and pins NaN to the origin", () => {
    expect(gridFromUnit(0.0005)).toBe(1); // 0.5 → 1, half up
    expect(gridFromUnit(0.00049)).toBe(0);
    expect(gridFromUnit(Number.NaN)).toBe(0);
    expect(gridFromUnit(-1)).toBe(0);
    expect(gridFromUnit(17)).toBe(FIELD_GRID);
  });
});

const doc = (nodes: unknown[]): CanvasDoc => parseCanvas({ nodes, edges: [] });

describe("fieldPlacements", () => {
  it("keys file nodes by source-relative path, first occurrence wins", () => {
    const d = doc([
      { id: "a", type: "file", x: 100, y: 200, width: 1, height: 1, file: "x.html" },
      { id: "b", type: "file", x: 900, y: 900, width: 1, height: 1, file: "x.html" },
      { id: "c", type: "file", x: 500, y: 0, width: 1, height: 1, file: "y.html" },
    ]);
    const p = fieldPlacements(d);
    expect(p.size).toBe(2);
    expect(p.get("x.html")).toEqual({ nodeId: "a", file: "x.html", x: 0.1, y: 0.2 });
    expect(p.get("y.html")?.x).toBe(0.5);
  });

  it("ignores non-file nodes and malformed file nodes", () => {
    const d = doc([
      { id: "g", type: "group", x: 0, y: 0, width: 10, height: 10, label: "isle" },
      { id: "t", type: "text", x: 0, y: 0, width: 1, height: 1, text: "hi" },
      { id: "bad", type: "file", x: "nope", y: 3, width: 1, height: 1, file: "z.html" },
      { id: "nofile", type: "file", x: 1, y: 1, width: 1, height: 1 },
    ]);
    expect(fieldPlacements(d).size).toBe(0);
  });
});

describe("fieldIslands", () => {
  it("reads operator-typed labels verbatim, in document order", () => {
    const d = doc([
      { id: "g1", type: "group", x: 0, y: 100, width: 300, height: 200, label: "dead ends" },
      { id: "f", type: "file", x: 0, y: 0, width: 1, height: 1, file: "a.html" },
      { id: "g2", type: "group", x: 500, y: 500, width: 100, height: 100 },
    ]);
    const isles = fieldIslands(d);
    expect(isles.map((i) => i.nodeId)).toEqual(["g1", "g2"]);
    expect(isles[0].label).toBe("dead ends");
    expect(isles[0]).toMatchObject({ x: 0, y: 0.1, w: 0.3, h: 0.2 });
    // No label typed by a hand ⇒ no label. Nothing generates one.
    expect(isles[1].label).toBeNull();
  });
});

describe("setPlacement", () => {
  it("appends a file node with a deterministic id and preserves the rest", () => {
    const d = parseCanvas({
      nodes: [{ id: "kbf1", type: "group", x: 0, y: 0, width: 4, height: 4, label: "i" }],
      edges: [{ id: "e1", fromNode: "kbf1", toNode: "kbf1" }],
      version: "obsidian-x",
    });
    const next = setPlacement(d, "docs/a.html", 0.25, 0.5);
    expect(nextFieldNodeId(d)).toBe("kbf2");
    expect(next.nodes).toHaveLength(2);
    expect(next.nodes[1]).toMatchObject({
      id: "kbf2",
      type: "file",
      x: 250,
      y: 500,
      file: "docs/a.html",
    });
    // Round-trip: unknown top-level keys and edges survive untouched.
    expect(next.edges).toEqual(d.edges);
    expect((next as Record<string, unknown>).version).toBe("obsidian-x");
  });

  it("moves the existing node for an already-placed artifact", () => {
    const d = doc([
      { id: "kbf1", type: "file", x: 10, y: 10, width: 2, height: 2, file: "a.html", note: "keep" },
    ]);
    const next = setPlacement(d, "a.html", 0.75, 0.125);
    expect(next.nodes).toHaveLength(1);
    expect(next.nodes[0]).toMatchObject({ id: "kbf1", x: 750, y: 125, note: "keep" });
  });

  it("clamps an off-field drop onto the field", () => {
    const next = setPlacement(doc([]), "a.html", -3, 9);
    expect(next.nodes[0]).toMatchObject({ x: 0, y: FIELD_GRID });
  });
});

// --- the recovered alignment -------------------------------------------

function row(
  id: string,
  raw: [number, number],
  aligned: [number, number],
  distance = 0,
): AtlasFieldDisagreementOut {
  return {
    id,
    machine_x: aligned[0],
    machine_y: aligned[1],
    operator_x: aligned[0],
    operator_y: aligned[1],
    operator_raw_x: raw[0],
    operator_raw_y: raw[1],
    distance,
  };
}

describe("recoverFieldTransform", () => {
  it("recovers a rotation + scale + translation exactly", () => {
    // 90° rotation, ×2 scale, +(0.1, -0.2) translation.
    const map = (x: number, y: number): [number, number] => [
      -2 * y + 0.1,
      2 * x - 0.2,
    ];
    const raws: [number, number][] = [
      [0.1, 0.2],
      [0.8, 0.3],
      [0.4, 0.9],
      [0.55, 0.05],
    ];
    const rows = raws.map((r, i) => row(`d${i}`, r, map(r[0], r[1])));
    const t = recoverFieldTransform(rows)!;
    expect(t).not.toBeNull();
    for (const [x, y] of raws) {
      const got = applyFieldTransform(t, x, y);
      const want = map(x, y);
      expect(got.x).toBeCloseTo(want[0], 6);
      expect(got.y).toBeCloseTo(want[1], 6);
    }
  });

  it("recovers a REFLECTED similarity too (procrustes may mirror)", () => {
    const map = (x: number, y: number): [number, number] => [y * 1.5, x * 1.5];
    const raws: [number, number][] = [
      [0.2, 0.1],
      [0.9, 0.4],
      [0.3, 0.8],
    ];
    const rows = raws.map((r, i) => row(`d${i}`, r, map(r[0], r[1])));
    const t = recoverFieldTransform(rows)!;
    const got = applyFieldTransform(t, 0.6, 0.7);
    expect(got.x).toBeCloseTo(1.05, 6);
    expect(got.y).toBeCloseTo(0.9, 6);
  });

  it("is null when the field can't determine one", () => {
    expect(recoverFieldTransform([])).toBeNull();
    expect(recoverFieldTransform([row("a", [0, 0], [0, 0])])).toBeNull();
    // collinear placements — no second independent direction
    expect(
      recoverFieldTransform([
        row("a", [0, 0], [0, 0]),
        row("b", [0.5, 0.5], [0.5, 0.5]),
        row("c", [0.9, 0.9], [0.9, 0.9]),
      ]),
    ).toBeNull();
    // all coincident
    expect(
      recoverFieldTransform([
        row("a", [0.3, 0.3], [0, 0]),
        row("b", [0.3, 0.3], [0, 0]),
        row("c", [0.3, 0.3], [0, 0]),
      ]),
    ).toBeNull();
  });

  it("is deterministic — same rows, same transform", () => {
    const rows = [
      row("a", [0.1, 0.9], [0.2, 0.8]),
      row("b", [0.5, 0.2], [0.6, 0.1]),
      row("c", [0.9, 0.4], [1.0, 0.3]),
    ];
    expect(recoverFieldTransform(rows)).toEqual(recoverFieldTransform(rows));
  });
});

describe("heat ramp", () => {
  it("normalises against the largest displacement, never NaN", () => {
    expect(heatT(0.5, 1)).toBe(0.5);
    expect(heatT(2, 1)).toBe(1);
    expect(heatT(-1, 1)).toBe(0);
    expect(heatT(1, 0)).toBe(0);
    expect(heatT(Number.NaN, 1)).toBe(0);
  });

  it("ramps cool → hot and clamps out-of-range t", () => {
    expect(heatColor(0)).toBe("rgb(91, 141, 239)");
    expect(heatColor(1)).toBe("rgb(226, 86, 74)");
    expect(heatColor(-5)).toBe(heatColor(0));
    expect(heatColor(5)).toBe(heatColor(1));
    expect(heatColor(0.5)).toBe("rgb(255, 183, 77)");
  });
});
