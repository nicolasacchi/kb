import { describe, expect, it } from "vitest";
import {
  classRank,
  compareNodes,
  EGO_COL_GAP,
  EGO_PAD_X,
  EGO_ROW_PITCH,
  layoutEgoGraph,
  layoutLayeredDag,
  type EgoLayoutInput,
} from "./egoGraph";

function baseInput(over: Partial<EgoLayoutInput> = {}): EgoLayoutInput {
  return {
    center: { id: "c", name: "center", class: "exact", path: "a.rs", line: 1 },
    inEdges: [
      { from: "caller-b", to: "c", class: "likely" },
      { from: "caller-a", to: "c", class: "exact" },
    ],
    outEdges: [
      { from: "c", to: "callee-z", class: "candidate" },
      { from: "c", to: "callee-m", class: "exact" },
    ],
    nodes: {
      "caller-a": { id: "caller-a", name: "alpha", class: "exact", path: "c.rs", line: 2 },
      "caller-b": { id: "caller-b", name: "beta", class: "likely", path: "c.rs", line: 3 },
      "callee-m": { id: "callee-m", name: "mid", class: "exact", path: "d.rs", line: 4 },
      "callee-z": { id: "callee-z", name: "zeta", class: "candidate", path: "d.rs", line: 5 },
    },
    depth: 1,
    nodeCap: 40,
    ...over,
  };
}

describe("classRank / compareNodes", () => {
  it("ranks exact < likely < candidate < other", () => {
    expect(classRank("exact")).toBeLessThan(classRank("likely"));
    expect(classRank("likely")).toBeLessThan(classRank("candidate"));
    expect(classRank("candidate")).toBeLessThan(classRank("other"));
  });

  it("orders by class then name", () => {
    const a = { id: "1", name: "z", class: "exact" };
    const b = { id: "2", name: "a", class: "likely" };
    const c = { id: "3", name: "m", class: "exact" };
    const sorted = [a, b, c].sort(compareNodes);
    expect(sorted.map((n) => n.id)).toEqual(["3", "1", "2"]); // exact m, exact z, likely a
  });
});

describe("layoutEgoGraph (deterministic layered grid)", () => {
  it("places center at layer 0, callers left, callees right", () => {
    const r = layoutEgoGraph(baseInput());
    const byId = Object.fromEntries(r.nodes.map((n) => [n.id, n]));
    expect(byId.c.layer).toBe(0);
    expect(byId["caller-a"].layer).toBe(-1);
    expect(byId["caller-b"].layer).toBe(-1);
    expect(byId["callee-m"].layer).toBe(1);
    expect(byId["callee-z"].layer).toBe(1);
    // X ordering: callers < center < callees
    expect(byId["caller-a"].x).toBeLessThan(byId.c.x);
    expect(byId.c.x).toBeLessThan(byId["callee-m"].x);
  });

  it("orders callers by class-rank then name (exact alpha before likely beta)", () => {
    const r = layoutEgoGraph(baseInput());
    const callers = r.nodes.filter((n) => n.layer === -1);
    expect(callers.map((n) => n.id)).toEqual(["caller-a", "caller-b"]);
  });

  it("orders callees by class-rank then name (exact mid before candidate zeta)", () => {
    const r = layoutEgoGraph(baseInput());
    const callees = r.nodes.filter((n) => n.layer === 1);
    expect(callees.map((n) => n.id)).toEqual(["callee-m", "callee-z"]);
  });

  it("is fully deterministic (identical input ⇒ identical output)", () => {
    const a = layoutEgoGraph(baseInput());
    const b = layoutEgoGraph(baseInput());
    expect(a).toEqual(b);
  });

  it("uses fixed column gap and row pitch", () => {
    const r = layoutEgoGraph(baseInput());
    const byId = Object.fromEntries(r.nodes.map((n) => [n.id, n]));
    expect(byId.c.x - byId["caller-a"].x).toBe(EGO_COL_GAP);
    expect(byId["callee-m"].x - byId.c.x).toBe(EGO_COL_GAP);
    // Two callers stacked: y differ by ROW_PITCH
    const callers = r.nodes.filter((n) => n.layer === -1);
    expect(callers[1].y - callers[0].y).toBe(EGO_ROW_PITCH);
    expect(byId["caller-a"].x).toBe(EGO_PAD_X);
  });

  it("respects nodeCap and reports truncated count", () => {
    // Cap = 3 → center + 2 neighbors (callers first, then callees).
    const r = layoutEgoGraph(baseInput({ nodeCap: 3 }));
    expect(r.nodes.length).toBe(3);
    expect(r.nodes.some((n) => n.id === "c")).toBe(true);
    // Both callers fit (2), callees truncated.
    expect(r.nodes.filter((n) => n.layer === -1).length).toBe(2);
    expect(r.nodes.filter((n) => n.layer === 1).length).toBe(0);
    expect(r.truncated).toBeGreaterThan(0);
  });

  it("golden: full snapshot of a small fixed graph", () => {
    const r = layoutEgoGraph(baseInput());
    // Strip floating geometry noise — assert the full structural golden.
    expect({
      truncated: r.truncated,
      nodes: r.nodes.map((n) => ({
        id: n.id,
        name: n.name,
        class: n.class,
        layer: n.layer,
        x: n.x,
        y: n.y,
      })),
      edges: r.edges,
    }).toEqual({
      truncated: 0,
      nodes: [
        { id: "caller-a", name: "alpha", class: "exact", layer: -1, x: 24, y: 40 },
        { id: "caller-b", name: "beta", class: "likely", layer: -1, x: 24, y: 88 },
        { id: "c", name: "center", class: "exact", layer: 0, x: 204, y: 64 },
        { id: "callee-m", name: "mid", class: "exact", layer: 1, x: 384, y: 40 },
        { id: "callee-z", name: "zeta", class: "candidate", layer: 1, x: 384, y: 88 },
      ],
      edges: [
        { from: "c", to: "callee-m", class: "exact" },
        { from: "c", to: "callee-z", class: "candidate" },
        { from: "caller-a", to: "c", class: "exact" },
        { from: "caller-b", to: "c", class: "likely" },
      ],
    });
  });

  it("depth 2 places outer callers at layer −2", () => {
    const r = layoutEgoGraph(
      baseInput({
        depth: 2,
        inEdges: [
          { from: "caller-a", to: "c", class: "exact" },
          { from: "outer-x", to: "caller-a", class: "likely" },
        ],
        outEdges: [],
        nodes: {
          "caller-a": { id: "caller-a", name: "alpha", class: "exact" },
          "outer-x": { id: "outer-x", name: "outer", class: "likely" },
        },
      }),
    );
    const outer = r.nodes.find((n) => n.id === "outer-x");
    expect(outer?.layer).toBe(-2);
    expect(outer!.x).toBeLessThan(r.nodes.find((n) => n.id === "caller-a")!.x);
  });
});

describe("layoutLayeredDag (review-map layout family)", () => {
  it("places dependencies left of dependents", () => {
    // a imports b → a depends on b → b left of a
    const r = layoutLayeredDag({
      nodes: [
        { id: "a.rs", name: "a.rs", path: "a.rs" },
        { id: "b.rs", name: "b.rs", path: "b.rs" },
      ],
      edges: [{ from: "a.rs", to: "b.rs", kind: "import", class: "exact" }],
    });
    const byId = Object.fromEntries(r.nodes.map((n) => [n.id, n]));
    expect(byId["b.rs"].layer).toBeLessThan(byId["a.rs"].layer);
    expect(byId["b.rs"].x).toBeLessThan(byId["a.rs"].x);
    expect(r.edges[0].kind).toBe("import");
  });

  it("is deterministic", () => {
    const input = {
      nodes: [
        { id: "z", name: "z" },
        { id: "a", name: "a" },
        { id: "m", name: "m" },
      ],
      edges: [
        { from: "z", to: "m", kind: "call", class: "likely" },
        { from: "m", to: "a", kind: "import", class: "exact" },
      ],
    };
    expect(layoutLayeredDag(input)).toEqual(layoutLayeredDag(input));
  });
});
