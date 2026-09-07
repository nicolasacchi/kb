// `lib/boards.ts` — the node→card mapping, walked over EVERY vocabulary
// (V74-L2).
//
// The property this file exists to pin is the one D10 states as a product
// rule: **every node renders, in every state**. So the walks below are
// exhaustive over `BOARD_NODE_KINDS` × `BOARD_NODE_STATES` ×
// `BOARD_NODE_REASONS` rather than sampling — a new kind or a new reason on the
// wire fails here (and in `boards.vocab.test.ts`, which is what proves the
// lists are the daemon's own) instead of rendering as a blank card.
import { describe, expect, it } from "vitest";
import type { BoardEdge, BoardNode, BoardOut } from "../api/types";
import {
  BOARD_EDGE_KINDS,
  BOARD_NODE_KINDS,
  BOARD_NODE_REASONS,
  BOARD_NODE_STATES,
  boardCaption,
  boardCensus,
  boardReasonLabel,
  boardStateLabel,
  cardKindFor,
  codeCardFor,
  contextCardFor,
  edgeClass,
  edgeTitle,
  hasContextExpansion,
  readingOrder,
} from "./boards";

function node(over: Partial<BoardNode> = {}): BoardNode {
  return {
    id: "n1",
    kind: "note",
    state: "inert",
    reason: "no-reference",
    address: "n1",
    ...over,
  };
}

function codeNode(over: Partial<BoardNode> = {}): BoardNode {
  return node({
    id: "c1",
    kind: "code",
    state: "pinned",
    reason: "blob-current",
    address: "app.rb:2-4",
    code: {
      path: "app.rb",
      range: [2, 4],
      authored_range: [2, 4],
      shifted_by: 0,
      snippet: "a\nb\nc",
      snippet_truncated: false,
      ...(over.code ?? {}),
    },
    ...over,
  });
}

function board(over: Partial<BoardOut> = {}): BoardOut {
  return {
    schema: "kbc-canvas/1",
    repo: "r",
    slug: "b",
    title: "T",
    description_md: "",
    status: "pending",
    revision: 1,
    content_hash: "h",
    created_unix: 0,
    updated_unix: 0,
    nodes: [],
    edges: [],
    steps: [],
    pins: {},
    honesty: {
      nodes: 0,
      edges: 0,
      steps: 0,
      pinned: 0,
      carried: 0,
      orphans: 0,
      present: 0,
      inert: 0,
      truncated_snippets: 0,
      stale_pins: 0,
      live_queries: false,
      budget: { max_nodes: 200, max_edges: 600, max_snippet_lines: 60 },
      notes: [],
    },
    ...over,
  };
}

describe("every node kind gets a card", () => {
  it("cardKindFor is TOTAL over the wire's kind vocabulary", () => {
    const seen = new Map<string, string>();
    for (const kind of BOARD_NODE_KINDS) {
      const got = cardKindFor(node({ kind }));
      expect(got, kind).toBeTruthy();
      seen.set(kind, got);
    }
    // The five kinds with a renderer of their own, and the five that share the
    // address card — stated as a table so a future kind has to choose.
    expect(Object.fromEntries(seen)).toEqual({
      code: "code",
      note: "note",
      query: "query",
      group: "group",
      link: "link",
      hunk: "address",
      finding: "address",
      annotation: "address",
      turn: "address",
      bookmark: "address",
    });
  });

  it("an UNKNOWN kind falls to the address card rather than vanishing", () => {
    expect(cardKindFor(node({ kind: "constellation" }))).toBe("address");
  });

  it("every (state, reason) pair produces a labelled card", () => {
    for (const state of BOARD_NODE_STATES) {
      for (const reason of BOARD_NODE_REASONS) {
        const n = node({ state, reason });
        expect(boardStateLabel(n.state), `${state}/${reason}`).not.toBe("");
        expect(boardCaption(n), `${state}/${reason}`).toContain(boardReasonLabel(reason));
      }
    }
  });
});

describe("the code card", () => {
  it("adapts onto the review document's card shape, with NO trust class", () => {
    const card = codeCardFor(codeNode());
    expect(card).not.toBeNull();
    // `trustTierFrom` classifies a missing class DOWN to `candidate`; passing
    // one through would be a claim the daemon never made for a board node.
    expect(card!.trust).toBeNull();
    expect(card!.scheme).toBe("code");
    expect(card!.path).toBe("app.rb");
    expect(card!.line).toBe(2);
    expect(card!.line_end).toBe(4);
    expect(card!.snippet_start).toBe(2);
  });

  it("a CARRIED card numbers its gutter where the bytes ARE, and says how far they moved", () => {
    const n = codeNode({
      state: "carried",
      reason: "reanchored-exact",
      code: {
        path: "app.rb",
        range: [9, 11],
        authored_range: [2, 4],
        shifted_by: 7,
        snippet: "a\nb\nc",
        snippet_truncated: false,
      },
    });
    const card = codeCardFor(n)!;
    expect(card.snippet_start).toBe(9);
    expect(card.caption).toContain("moved down 7 lines");
  });

  it("an upward move says UP, and a one-line move is singular", () => {
    const up = codeNode({
      code: {
        path: "app.rb",
        range: [1, 1],
        authored_range: [2, 2],
        shifted_by: -1,
        snippet: "a",
        snippet_truncated: false,
      },
    });
    expect(boardCaption(up)).toContain("moved up 1 line");
    expect(boardCaption(up)).not.toContain("1 lines");
  });

  it("an ORPHAN still has a card — with no snippet and its last-known text", () => {
    const n = codeNode({
      state: "orphan",
      reason: "path-gone",
      code: {
        path: "gone.rb",
        range: [2, 4],
        authored_range: [2, 4],
        shifted_by: 0,
        snippet: null,
        snippet_truncated: false,
        anchor_snippet: "def hello",
      },
    });
    const card = codeCardFor(n)!;
    expect(card.state).toBe("orphan");
    expect(card.snippet).toBeNull();
    expect(card.caption).toContain("the file is gone");
  });

  it("a non-code node has no code card", () => {
    expect(codeCardFor(node({ kind: "note" }))).toBeNull();
  });
});

describe("the ± context expansion", () => {
  const withCtx = codeNode({
    code: {
      path: "app.rb",
      range: [2, 4],
      context: [1, 6],
      authored_range: [2, 4],
      shifted_by: 0,
      snippet: "a\nb\nc",
      snippet_truncated: false,
      context_snippet: "z\na\nb\nc\nd\ne",
      highlights: [],
    },
  });

  it("is available only when the daemon actually SENT one", () => {
    expect(hasContextExpansion(withCtx)).toBe(true);
    // A context RANGE with no fetched text is not an expansion: the read did
    // not ask for it, and synthesising the surrounding lines is the one thing
    // this surface may not do.
    const rangeOnly = codeNode({
      code: {
        path: "app.rb",
        range: [2, 4],
        context: [1, 6],
        authored_range: [2, 4],
        shifted_by: 0,
        snippet: "a\nb\nc",
        snippet_truncated: false,
      },
    });
    expect(hasContextExpansion(rangeOnly)).toBe(false);
    expect(contextCardFor(rangeOnly)).toBeNull();
  });

  it("renders the CONTEXT range's own numbers and drops the primary's spans", () => {
    const card = contextCardFor(withCtx)!;
    expect(card.line).toBe(1);
    expect(card.line_end).toBe(6);
    expect(card.snippet_start).toBe(1);
    expect(card.snippet).toBe("z\na\nb\nc\nd\ne");
    // The daemon rebases spans onto the PRIMARY snippet; reusing them here
    // would paint the wrong bytes, so the expansion renders unpainted.
    expect(card.highlights).toBeNull();
  });
});

describe("edges", () => {
  function edge(over: Partial<BoardEdge> = {}): BoardEdge {
    return { from: "a", to: "b", kind: "calls", provenance: "authored", ...over };
  }

  it("an AUTHORED edge gets one stroke and NO trust class", () => {
    const cls = edgeClass(edge({ trust: "exact" }));
    expect(cls).toContain("kbc-board__edge--authored");
    expect(cls).not.toContain("kbc-trust-");
    expect(edgeTitle(edge())).toContain("authored");
  });

  it("a DERIVED edge carries the class it inherited, in the trust lane", () => {
    expect(edgeClass(edge({ provenance: "derived", trust: "likely" }))).toContain(
      "kbc-trust-likely",
    );
    // A derived edge with no class is the weakest tier, not the strongest —
    // classifying UP would be the trust laundering the whole rule prevents.
    expect(edgeClass(edge({ provenance: "derived" }))).toContain("kbc-trust-candidate");
    expect(edgeTitle(edge({ provenance: "derived" }))).toContain("unclassed");
  });

  it("every edge kind gets its own class", () => {
    for (const kind of BOARD_EDGE_KINDS) {
      expect(edgeClass(edge({ kind })), kind).toContain(`kbc-board__edge--${kind}`);
    }
  });

  it("an UNKNOWN edge kind still draws, as the neutral `then` stroke", () => {
    expect(edgeClass(edge({ kind: "teleports" }))).toContain("kbc-board__edge--then");
  });
});

describe("the census and the reading order", () => {
  it("reports the wire's numbers verbatim and never a length", () => {
    const b = board({
      nodes: [node(), codeNode()],
      honesty: {
        ...board().honesty,
        nodes: 40,
        edges: 3,
        steps: 2,
        pinned: 10,
        carried: 4,
        orphans: 2,
        present: 20,
        inert: 4,
        truncated_snippets: 1,
        stale_pins: 1,
        live_queries: false,
      },
    });
    const lines = boardCensus(b).join(" | ");
    // 40, not `nodes.length` (2) — the honesty block is the only source.
    expect(lines).toContain("40 nodes");
    expect(lines).toContain("2 orphans");
    expect(lines).toContain("shown, never dropped");
    expect(lines).toContain("cut at the 60-line cap");
    expect(lines).toContain("stale pin");
    expect(lines).toContain("query counts are as authored");
  });

  it("says nothing about live counts once they ARE live", () => {
    const b = board({ honesty: { ...board().honesty, live_queries: true } });
    expect(boardCensus(b).join(" ")).not.toContain("as authored");
  });

  it("reading order is steps first, then wire order, with nothing lost or doubled", () => {
    const b = board({
      nodes: [node({ id: "a" }), node({ id: "b" }), node({ id: "c" })],
      steps: [{ node: "c" }, { node: "a" }, { node: "c" }],
    });
    expect(readingOrder(b).map((n) => n.id)).toEqual(["c", "a", "b"]);
  });

  it("a step naming a node that is not on the board is skipped, not rendered empty", () => {
    const b = board({ nodes: [node({ id: "a" })], steps: [{ node: "ghost" }, { node: "a" }] });
    expect(readingOrder(b).map((n) => n.id)).toEqual(["a"]);
  });
});
