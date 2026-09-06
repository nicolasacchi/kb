import { describe, expect, it } from "vitest";
import {
  addNode,
  defaultLayoutFor,
  DEFAULT_CARD_HEIGHT,
  DEFAULT_CARD_WIDTH,
  EMPTY_CANVAS,
  moveNode,
  parseCanvas,
  placedFiles,
  removeNode,
  serializeCanvas,
  type CanvasDoc,
} from "./canvas";

describe("parseCanvas", () => {
  it("defaults a bare object to empty nodes/edges arrays", () => {
    expect(parseCanvas({})).toEqual(EMPTY_CANVAS);
  });

  it("defaults non-object input (absent-file GET) without throwing", () => {
    expect(parseCanvas(null)).toEqual(EMPTY_CANVAS);
    expect(parseCanvas(undefined)).toEqual(EMPTY_CANVAS);
    expect(parseCanvas("not an object")).toEqual(EMPTY_CANVAS);
    expect(parseCanvas([1, 2, 3])).toEqual(EMPTY_CANVAS);
  });

  it("defaults a missing/non-array nodes or edges field independently", () => {
    expect(parseCanvas({ edges: [] })).toEqual({ nodes: [], edges: [] });
    expect(parseCanvas({ nodes: [], edges: "oops" })).toEqual({
      nodes: [],
      edges: [],
    });
  });
});

describe("round-trip preserves unknown fields (golden)", () => {
  it("keeps an unrecognized top-level key through parse → serialize", () => {
    const raw = {
      nodes: [],
      edges: [],
      // A hypothetical future JSON Canvas top-level field, or metadata
      // another tool stamped — kb must not silently drop it.
      metadata: { app: "obsidian", version: "1.0" },
    };
    const doc = parseCanvas(raw);
    const rebuilt = JSON.parse(serializeCanvas(doc));
    expect(rebuilt).toEqual(raw);
  });

  it("keeps unrecognized per-node and per-edge fields untouched by a targeted move", () => {
    const raw = {
      nodes: [
        {
          id: "a",
          type: "file",
          x: 0,
          y: 0,
          width: 260,
          height: 120,
          file: "foo.html",
          // an extension kb doesn't model:
          styleAttributes: { border: "dashed" },
        },
      ],
      edges: [
        {
          id: "e1",
          fromNode: "a",
          toNode: "a",
          // ditto:
          futureField: 42,
        },
      ],
    };
    const doc = parseCanvas(raw);
    const moved = moveNode(doc, "a", 500, 500);
    const rebuilt = JSON.parse(serializeCanvas(moved)) as CanvasDoc;
    // The moved node's position changed…
    expect(rebuilt.nodes[0].x).toBe(500);
    expect(rebuilt.nodes[0].y).toBe(500);
    // …but everything else survived byte-for-byte.
    expect(rebuilt.nodes[0].styleAttributes).toEqual({ border: "dashed" });
    expect(rebuilt.edges[0].futureField).toBe(42);
  });
});

describe("moveNode / addNode / removeNode", () => {
  const base: CanvasDoc = {
    nodes: [
      { id: "a", type: "file", x: 0, y: 0, width: 10, height: 10, file: "a.html" },
      { id: "b", type: "text", x: 100, y: 0, width: 10, height: 10, text: "hi" },
    ],
    edges: [{ id: "e1", fromNode: "a", toNode: "b" }],
  };

  it("moveNode only changes the targeted node's x/y", () => {
    const out = moveNode(base, "b", 7, 8);
    expect(out.nodes[0]).toEqual(base.nodes[0]);
    expect(out.nodes[1]).toMatchObject({ id: "b", x: 7, y: 8, text: "hi" });
  });

  it("moveNode on an unknown id is a no-op", () => {
    const out = moveNode(base, "nope", 1, 1);
    expect(out.nodes).toEqual(base.nodes);
  });

  it("addNode appends without disturbing existing nodes", () => {
    const node = { id: "c", type: "file" as const, x: 1, y: 1, width: 1, height: 1 };
    const out = addNode(base, node);
    expect(out.nodes).toHaveLength(3);
    expect(out.nodes[2]).toEqual(node);
    expect(out.nodes[0]).toBe(base.nodes[0]);
  });

  it("removeNode drops the node AND any edge touching it", () => {
    const out = removeNode(base, "a");
    expect(out.nodes.map((n) => n.id)).toEqual(["b"]);
    expect(out.edges).toEqual([]);
  });
});

describe("placedFiles", () => {
  it("collects only file-node paths, ignoring text/link/group nodes", () => {
    const doc: CanvasDoc = {
      nodes: [
        { id: "a", type: "file", x: 0, y: 0, width: 1, height: 1, file: "a.html" },
        { id: "b", type: "text", x: 0, y: 0, width: 1, height: 1, text: "note" },
        { id: "c", type: "file", x: 0, y: 0, width: 1, height: 1, file: "b.html" },
      ],
      edges: [],
    };
    expect(placedFiles(doc)).toEqual(new Set(["a.html", "b.html"]));
  });

  it("allows multi-placement — the same file on two nodes counts once in the set", () => {
    const doc: CanvasDoc = {
      nodes: [
        { id: "a", type: "file", x: 0, y: 0, width: 1, height: 1, file: "dup.html" },
        { id: "b", type: "file", x: 200, y: 0, width: 1, height: 1, file: "dup.html" },
      ],
      edges: [],
    };
    expect(doc.nodes).toHaveLength(2);
    expect(placedFiles(doc)).toEqual(new Set(["dup.html"]));
  });
});

describe("defaultLayoutFor (determinism golden)", () => {
  it("is a pure function of input order — same input, same output", () => {
    const entries = [
      { source_relative: "a.html" },
      { source_relative: "b.html" },
      { source_relative: "c.html" },
    ];
    expect(defaultLayoutFor(entries)).toEqual(defaultLayoutFor(entries));
    expect(defaultLayoutFor([...entries])).toEqual(defaultLayoutFor(entries));
  });

  it("lays out a 4-column grid at fixed card size, in list-position order", () => {
    const entries = Array.from({ length: 5 }, (_, i) => ({
      source_relative: `doc-${i}.html`,
    }));
    const doc = defaultLayoutFor(entries);
    expect(doc.edges).toEqual([]);
    expect(doc.nodes).toHaveLength(5);
    // Row 0: 4 columns across.
    expect(doc.nodes[0]).toMatchObject({ x: 0, y: 0, file: "doc-0.html" });
    expect(doc.nodes[1]).toMatchObject({
      x: DEFAULT_CARD_WIDTH + 40,
      y: 0,
      file: "doc-1.html",
    });
    expect(doc.nodes[3]).toMatchObject({
      x: 3 * (DEFAULT_CARD_WIDTH + 40),
      y: 0,
      file: "doc-3.html",
    });
    // Row 1: wraps after GRID_COLUMNS (4).
    expect(doc.nodes[4]).toMatchObject({
      x: 0,
      y: DEFAULT_CARD_HEIGHT + 40,
      file: "doc-4.html",
    });
    // Every node keeps the fixed card size.
    for (const n of doc.nodes) {
      expect(n.width).toBe(DEFAULT_CARD_WIDTH);
      expect(n.height).toBe(DEFAULT_CARD_HEIGHT);
      expect(n.type).toBe("file");
    }
  });

  it("skips entries with no resolvable source_relative (e.g. a tombstoned artifact)", () => {
    const doc = defaultLayoutFor([
      { source_relative: "a.html" },
      { source_relative: null },
      {},
      { source_relative: "b.html" },
    ]);
    expect(doc.nodes.map((n) => n.file)).toEqual(["a.html", "b.html"]);
  });

  it("returns the byte-identical empty doc for no entries", () => {
    expect(defaultLayoutFor([])).toEqual(EMPTY_CANVAS);
  });
});
