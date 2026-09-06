import { describe, expect, it } from "vitest";
import type { ActionOp, ActionRow, ActionTarget, ActionsOut } from "../api/types";
import {
  filterGroups,
  isAutoNavigable,
  menuAccessibleName,
  menuOrder,
  pillRows,
  resolveOp,
  snippetWithProvenance,
  targetAddress,
} from "./actionOps";

const ctx = { repo: "kb", origin: "https://kbc.example", ref: undefined };

function target(over: Partial<ActionTarget> = {}): ActionTarget {
  return {
    kind: "symbol",
    label: "Order#total",
    path: "app/models/order.rb",
    line: 88,
    col: 4,
    name: "total",
    blob_sha: "9f2cabc",
    ...over,
  };
}

function row(op: ActionOp, over: Partial<ActionRow> = {}): ActionRow {
  return {
    id: "x",
    version: 1,
    group: "find",
    title: "X",
    doc: "d",
    enabled: true,
    mutating: false,
    auto_navigate: false,
    op,
    ...over,
  };
}

describe("resolveOp is exhaustive and builds no URL of its own", () => {
  it("every wire op variant resolves to a component action", () => {
    const ops: ActionOp[] = [
      { op: "open", path: "a.rb", line: 3, pane: 1 },
      { op: "peek", kind: "definition" },
      { op: "dock", dock: "usages" },
      { op: "search", query: "/total" },
      { op: "copy", what: "cli", value: "kb-code act --list a.rb" },
      { op: "compose", surface: "ask" },
      { op: "collect", sink: "bookmark" },
    ];
    for (const op of ops) {
      const r = resolveOp(row(op), target(), ctx);
      expect(r.kind, op.op).not.toBe("unavailable");
    }
  });

  it("an open op routes through codeUrl, never a hand-built href", () => {
    const r = resolveOp(row({ op: "open", path: "app/a.rb", line: 12, pane: 2 }), target(), ctx);
    expect(r).toEqual({ kind: "navigate", href: "/r/kb/app/a.rb?line=12", pane: 2 });
  });

  it("a permalink pins the blob sha the target was read at", () => {
    const r = resolveOp(row({ op: "copy", what: "permalink" }), target(), ctx);
    expect(r.kind).toBe("clipboard");
    if (r.kind !== "clipboard") return;
    expect(r.text).toBe("https://kbc.example/r/kb/app/models/order.rb?ref=9f2cabc&line=88");
  });

  it("a range permalink carries the whole range", () => {
    const r = resolveOp(
      row({ op: "copy", what: "permalink" }),
      target({ kind: "range", line: 88, end_line: 92 }),
      ctx,
    );
    if (r.kind !== "clipboard") throw new Error("expected clipboard");
    expect(r.text).toContain("line=88-92");
  });

  it("a snippet with nothing selected REFUSES with a reason, never a silent no-op", () => {
    const r = resolveOp(row({ op: "copy", what: "snippet" }), target({ kind: "range" }), ctx);
    expect(r).toEqual({
      kind: "unavailable",
      reason: "nothing is selected — a provenance snippet needs the lines it quotes",
    });
  });

  it("a snippet carries path, sha and range in its header", () => {
    const r = resolveOp(
      row({ op: "copy", what: "snippet" }),
      target({ kind: "range", line: 88, end_line: 92 }),
      { ...ctx, selectedText: "def total\nend" },
    );
    if (r.kind !== "clipboard") throw new Error("expected clipboard");
    expect(r.text).toBe("// app/models/order.rb:88-92 @9f2cabc\ndef total\nend");
  });

  it("a snippet with no blob sha SAYS so rather than dropping the header", () => {
    expect(snippetWithProvenance(target({ blob_sha: undefined }), "x")).toContain(
      "(blob sha unknown)",
    );
  });

  it("a sym copy on a target with no name refuses by name", () => {
    const r = resolveOp(row({ op: "copy", what: "sym" }), target({ name: undefined }), ctx);
    expect(r).toEqual({ kind: "unavailable", reason: "this target has no symbol name" });
  });
});

describe("D5's standing rules, as predicates", () => {
  it("nothing on this wire auto-navigates", () => {
    expect(isAutoNavigable(row({ op: "peek", kind: "definition" }))).toBe(false);
  });

  it("targetAddress renders a range as a range and a caret as a line", () => {
    expect(targetAddress(target())).toBe("app/models/order.rb:88");
    expect(targetAddress(target({ line: 88, end_line: 92 }))).toBe("app/models/order.rb:88-92");
    expect(targetAddress(target({ line: undefined }))).toBe("app/models/order.rb");
  });

  it("the accessible name states the target, which no screen reader can infer", () => {
    expect(menuAccessibleName(target(), 7)).toBe("Actions for symbol Order#total, 7 items");
    expect(menuAccessibleName(target(), 1)).toBe("Actions for symbol Order#total, 1 item");
  });
});

describe("the drag-select pill is DERIVED from the same list", () => {
  const out: ActionsOut = {
    schema: "kbc-actions/1",
    repo: "kb",
    targets: [target()],
    active: 0,
    groups: [
      {
        id: "navigate",
        title: "Go",
        actions: [
          row({ op: "peek", kind: "definition" }, { id: "a" }),
          row({ op: "dock", dock: "definitions" }, { id: "b", enabled: false }),
        ],
      },
      {
        id: "find",
        title: "Find",
        actions: [
          row({ op: "dock", dock: "usages" }, { id: "c", title: "Usages" }),
          row({ op: "search", query: "/x" }, { id: "d" }),
          row({ op: "search", query: "/y" }, { id: "e" }),
        ],
      },
    ],
    mutations: { available: false, reason: "not loopback" },
    notes: [],
  };

  it("takes the top three ENABLED rows in server order", () => {
    expect(pillRows(out).map((r) => r.id)).toEqual(["a", "c", "d"]);
  });

  it("renders fewer than three without complaint", () => {
    expect(pillRows({ ...out, groups: [out.groups[0]] }).map((r) => r.id)).toEqual(["a"]);
  });

  it("the menu's keyboard order is groups-then-rows, in server order", () => {
    expect(menuOrder(out.groups).map((r) => r.id)).toEqual(["a", "b", "c", "d", "e"]);
  });

  it("type-to-filter is a plain substring, never a second ranking", () => {
    expect(menuOrder(filterGroups(out, "usages")).map((r) => r.id)).toEqual(["c"]);
    expect(menuOrder(filterGroups(out, "")).length).toBe(5);
    expect(filterGroups(out, "no-such-row")).toHaveLength(0);
  });
});
