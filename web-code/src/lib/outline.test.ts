import { describe, expect, it } from "vitest";
import type { Symbol } from "../api/types";
import { buildOutline, flattenOutline, symbolAtLine } from "./outline";

function sym(overrides: Partial<Symbol> & { ordinal: number; name: string }): Symbol {
  return {
    kind: "function",
    line_start: overrides.ordinal,
    line_end: overrides.ordinal,
    col_start: 0,
    col_end: 0,
    container: null,
    signature: null,
    doc: null,
    ...overrides,
  };
}

describe("buildOutline", () => {
  it("treats every symbol as a root when none has a container", () => {
    const symbols = [sym({ ordinal: 0, name: "a" }), sym({ ordinal: 1, name: "b" })];
    const roots = buildOutline(symbols);
    expect(roots.map((r) => r.symbol.name)).toEqual(["a", "b"]);
    expect(roots[0].children).toEqual([]);
  });

  it("nests a symbol under its container", () => {
    const symbols = [
      sym({ ordinal: 0, name: "Widget", kind: "struct" }),
      sym({ ordinal: 1, name: "new", kind: "method", container: "Widget" }),
    ];
    const roots = buildOutline(symbols);
    expect(roots).toHaveLength(1);
    expect(roots[0].symbol.name).toBe("Widget");
    expect(roots[0].children.map((c) => c.symbol.name)).toEqual(["new"]);
  });

  it("falls back to a root node when the named container isn't in this file's symbol list", () => {
    const symbols = [sym({ ordinal: 0, name: "orphan_method", container: "NotInThisFile" })];
    const roots = buildOutline(symbols);
    expect(roots.map((r) => r.symbol.name)).toEqual(["orphan_method"]);
  });

  it("supports multi-level nesting (yaml/toml key-paths)", () => {
    const symbols = [
      sym({ ordinal: 0, name: "spec", kind: "key" }),
      sym({ ordinal: 1, name: "spec.containers", kind: "key", container: "spec" }),
      sym({
        ordinal: 2,
        name: "spec.containers.image",
        kind: "key",
        container: "spec.containers",
      }),
    ];
    const roots = buildOutline(symbols);
    expect(roots).toHaveLength(1);
    expect(roots[0].children[0].children.map((c) => c.symbol.name)).toEqual([
      "spec.containers.image",
    ]);
  });
});

describe("flattenOutline", () => {
  it("flattens a nested tree with correct depths in ordinal order", () => {
    const symbols = [
      sym({ ordinal: 0, name: "Widget", kind: "struct" }),
      sym({ ordinal: 1, name: "new", kind: "method", container: "Widget" }),
      sym({ ordinal: 2, name: "Other", kind: "struct" }),
    ];
    const flat = flattenOutline(buildOutline(symbols));
    expect(flat.map((f) => [f.symbol.name, f.depth])).toEqual([
      ["Widget", 0],
      ["new", 1],
      ["Other", 0],
    ]);
  });

  it("is empty for an empty tree", () => {
    expect(flattenOutline([])).toEqual([]);
  });
});

describe("symbolAtLine", () => {
  const symbols = [
    sym({ ordinal: 0, name: "outer", line_start: 1, line_end: 20 }),
    sym({ ordinal: 1, name: "inner", line_start: 5, line_end: 8, container: "outer" }),
  ];

  it("returns the most specific (smallest) enclosing symbol", () => {
    expect(symbolAtLine(symbols, 6)?.name).toBe("inner");
  });

  it("falls back to the wider enclosing symbol outside the inner range", () => {
    expect(symbolAtLine(symbols, 15)?.name).toBe("outer");
  });

  it("returns null when no symbol contains the line", () => {
    expect(symbolAtLine(symbols, 100)).toBeNull();
  });
});
