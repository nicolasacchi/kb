// `lib/boardDoc.ts` — composing the next board document (V74-L2).
//
// The load-bearing assertion is the NEGATIVE one: a document this module builds
// contains no coordinate-shaped key outside `pins`, ever. Everything else here
// walks the lint's own per-kind reference table so a round trip cannot smuggle
// a computed field back to a daemon that would refuse it.
import { describe, expect, it } from "vitest";
import type { ActionTarget, BoardNode, BoardOut } from "../api/types";
import { BOARD_NODE_KINDS } from "./boards";
import {
  APPLIABLE_STATUSES,
  BOARD_SCHEMA,
  REF_FIELDS_BY_KIND,
  appliableStatus,
  boardToDoc,
  composeAdd,
  composeNew,
  composePin,
  composeThread,
  coordinateKeysOutsidePins,
  edgeToInput,
  isValidBoardId,
  lintSummary,
  mintNodeId,
  nodeFromTarget,
  nodeToInput,
  threadAnchorFor,
} from "./boardDoc";

function board(over: Partial<BoardOut> = {}): BoardOut {
  return {
    schema: BOARD_SCHEMA,
    repo: "r",
    slug: "b",
    title: "T",
    description_md: "why",
    status: "pending",
    revision: 3,
    content_hash: "h",
    created_unix: 1,
    updated_unix: 2,
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

function target(over: Partial<ActionTarget> = {}): ActionTarget {
  return { kind: "range", label: "app.rb:2-4", path: "app.rb", line: 2, end_line: 4, ...over };
}

describe("the coordinate refusal", () => {
  it("finds a coordinate key anywhere OUTSIDE pins, by path", () => {
    expect(coordinateKeysOutsidePins({ nodes: [{ id: "a", x: 1 }] })).toEqual(["nodes[0].x"]);
    expect(coordinateKeysOutsidePins({ a: { b: { position: "left" } } })).toEqual([
      "a.b.position",
    ]);
  });

  it("permits the pins map, which is the ONE authored geometry", () => {
    expect(coordinateKeysOutsidePins({ pins: { n1: { x: 1, y: 2 } } })).toEqual([]);
  });

  it("does NOT permit a nested `pins` masquerading as the top-level one", () => {
    expect(coordinateKeysOutsidePins({ nodes: [{ pins: { x: 1 } }] })).toEqual([
      "nodes[0].pins.x",
    ]);
  });

  it("every composition this module produces is clean", () => {
    const b = board({ nodes: [], pins: { n1: { x: 5, y: 6 } } });
    expect(composeAdd(b, target()).coordinateViolations).toEqual([]);
    expect(composeNew("r", "s", "T", target()).coordinateViolations).toEqual([]);
    expect(composePin(b, "n1", { x: 9, y: 9 }).coordinateViolations).toEqual([]);
  });
});

describe("round-tripping a node", () => {
  function node(kind: string, extra: Partial<BoardNode> = {}): BoardNode {
    return {
      id: "n1",
      kind,
      state: "present",
      reason: "target-present",
      address: "a",
      ...extra,
    };
  }

  it("keeps ONLY the authored half — never the daemon's resolution", () => {
    const n = node("code", {
      path: "app.rb",
      range: [2, 4],
      symbol: "hello",
      blob_sha: "deadbeef",
      code: {
        path: "app.rb",
        range: [9, 11],
        authored_range: [2, 4],
        shifted_by: 7,
        snippet: "x",
        snippet_truncated: false,
      },
      thread: { id: "an-1", resolved: false, replies: 2 },
      pin: { x: 1, y: 2 },
      note: "a daemon caption",
    });
    const out = nodeToInput(n);
    expect(out).toEqual({
      id: "n1",
      kind: "code",
      thread_id: "an-1",
      path: "app.rb",
      symbol: "hello",
      range: [2, 4],
      blob_sha: "deadbeef",
    });
    // The RESOLVED range never goes back: re-sending `[9, 11]` as the author's
    // own claim would launder a carried card into a pinned one.
    expect(JSON.stringify(out)).not.toContain("9");
    // …and neither does the pin, which travels in the document's `pins` map.
    expect(out).not.toHaveProperty("pin");
  });

  it("carries exactly the fields the lint's per-kind table allows, for every kind", () => {
    const everything: Partial<BoardNode> = {
      path: "p",
      symbol: "s",
      range: [1, 2],
      context: [1, 3],
      blob_sha: "b",
      guard_hash: "g",
      query: "q",
      authored_count: 1,
      review: 7,
      patchset: 2,
      hunk: "h",
      finding: "f-x",
      annotation: "a",
      session: "sess",
      turn: "t-abc",
      bookmark: 4,
      members: ["n2"],
      url: "https://example.test",
    };
    for (const kind of BOARD_NODE_KINDS) {
      const out = nodeToInput(node(kind, everything));
      const carried = Object.keys(out).filter((k) => !["id", "kind"].includes(k));
      expect(carried.sort(), kind).toEqual([...(REF_FIELDS_BY_KIND[kind] ?? [])].sort());
    }
  });

  it("an edge keeps its trust class only when it is DERIVED", () => {
    expect(
      edgeToInput({ from: "a", to: "b", kind: "calls", provenance: "authored", trust: "exact" }),
    ).toEqual({ from: "a", to: "b", kind: "calls", provenance: "authored" });
    expect(
      edgeToInput({ from: "a", to: "b", kind: "calls", provenance: "derived", trust: "likely" }),
    ).toMatchObject({ trust: "likely" });
  });

  it("the whole board round-trips into a document the lint would accept", () => {
    const b = board({
      nodes: [node("note", { body_md: "hi", title: "t" })],
      edges: [{ from: "n1", to: "n1", kind: "then", provenance: "authored" }],
      steps: [{ node: "n1", caption: "start" }],
      pins: { n1: { x: 1, y: 2 } },
      authored_ref: "main",
    });
    const doc = boardToDoc(b);
    expect(doc.schema).toBe(BOARD_SCHEMA);
    expect(doc.authored_ref).toBe("main");
    expect(doc.pins).toEqual({ n1: { x: 1, y: 2 } });
    expect(coordinateKeysOutsidePins(doc)).toEqual([]);
  });
});

describe("status", () => {
  it("apply may only write an appliable status", () => {
    for (const s of APPLIABLE_STATUSES) expect(appliableStatus(s)).toBe(s);
    expect(appliableStatus("accepted")).toBe("pending");
    expect(appliableStatus("archived")).toBe("pending");
  });

  it("adding to an ACCEPTED board reports the reset before the click", () => {
    expect(composeAdd(board({ status: "accepted" }), target()).statusWillReset).toBe(true);
    expect(composeAdd(board({ status: "pending" }), target()).statusWillReset).toBe(false);
  });
});

describe("minting a node from an action target", () => {
  it("a range target becomes a code node over the range it names", () => {
    const { node, notes } = nodeFromTarget(target({ blob_sha: "abc" }), []);
    expect(node.kind).toBe("code");
    expect(node.path).toBe("app.rb");
    expect(node.range).toEqual([2, 4]);
    expect(node.blob_sha).toBe("abc");
    expect(notes).toEqual([]);
  });

  it("a symbol target keeps the symbol beside the range", () => {
    const { node } = nodeFromTarget(
      target({ kind: "symbol", name: "Greeter#call", line: 5, end_line: undefined }),
      [],
    );
    expect(node.symbol).toBe("Greeter#call");
    expect(node.range).toEqual([5, 5]);
  });

  it("a LINE-LESS path target anchors at line 1 and SAYS so", () => {
    const { node, notes } = nodeFromTarget(
      target({ kind: "path", line: undefined, end_line: undefined, blob_sha: "z" }),
      [],
    );
    expect(node.range).toEqual([1, 1]);
    expect(notes.join(" ")).toContain("anchors at line 1");
  });

  it("names the two things it could not establish, rather than hiding them", () => {
    const { notes } = nodeFromTarget(target({ exists: false }), []);
    expect(notes.join(" ")).toContain("no blob sha");
    expect(notes.join(" ")).toContain("honest orphan");
  });

  it("mints a slug-safe id and de-duplicates against the board", () => {
    expect(isValidBoardId(mintNodeId("App/Models/Order.rb:12", []))).toBe(true);
    expect(mintNodeId("a", ["a"])).toBe("a-2");
    expect(mintNodeId("a", ["a", "a-2"])).toBe("a-3");
    // A seed with nothing usable in it still produces a legal id.
    expect(isValidBoardId(mintNodeId("!!!", []))).toBe(true);
    // …and one that starts with a digit is fine (the daemon allows it).
    expect(isValidBoardId(mintNodeId("12-foo", []))).toBe(true);
  });

  it("a new board is never empty — the entry action brings the card", () => {
    const out = composeNew("r", "checkout-flow", "Checkout", target());
    expect(out.doc.nodes).toHaveLength(1);
    expect(out.doc.status).toBe("pending");
    expect(out.doc.edges).toEqual([]);
    expect(out.doc.pins).toEqual({});
  });
});

describe("pins and threads", () => {
  const b = board({
    nodes: [
      {
        id: "n1",
        kind: "code",
        state: "pinned",
        reason: "blob-current",
        address: "a",
        path: "app.rb",
        range: [2, 4],
        code: {
          path: "app.rb",
          range: [2, 4],
          authored_range: [2, 4],
          shifted_by: 0,
          snippet_truncated: false,
        },
      },
    ],
    pins: { n1: { x: 3, y: 4 } },
  });

  it("a pin is rounded and lands in the pins map — the ONE geometry door", () => {
    const { doc } = composePin(b, "n1", { x: 3.7, y: -2.2 });
    expect(doc.pins.n1).toEqual({ x: 4, y: -2 });
    expect(coordinateKeysOutsidePins(doc)).toEqual([]);
  });

  it("unpinning deletes the entry rather than writing a zero", () => {
    const { doc } = composePin(b, "n1", null);
    expect(doc.pins).toEqual({});
  });

  it("a thread id is recorded on the node it belongs to", () => {
    const { doc } = composeThread(b, "n1", "an-9");
    expect(doc.nodes[0].thread_id).toBe("an-9");
  });

  it("a thread anchors only where a path AND a line exist", () => {
    expect(threadAnchorFor(b.nodes[0])).toEqual({ path: "app.rb", line: 2, lineEnd: 4 });
    expect(
      threadAnchorFor({
        id: "n2",
        kind: "note",
        state: "inert",
        reason: "no-reference",
        address: "n2",
      }),
    ).toBeNull();
    expect(
      threadAnchorFor({
        id: "n3",
        kind: "turn",
        state: "present",
        reason: "target-present",
        address: "session s turn t",
        session: "s",
        turn: "t-abc",
      }),
    ).toBeNull();
  });
});

describe("the refusal path", () => {
  it("renders the daemon's rule id and message verbatim", () => {
    expect(
      lintSummary([
        { rule: "coordinates", at: "nodes[0].x", message: "boards are coordinate-free" },
        { rule: "node-cap", message: "201 nodes; the cap is 200" },
      ]),
    ).toEqual([
      "[coordinates] nodes[0].x: boards are coordinate-free",
      "[node-cap] 201 nodes; the cap is 200",
    ]);
  });
});
