import { describe, expect, it } from "vitest";
import type { TreeEntry } from "../api/types";
import {
  ancestorDirs,
  buildTreeRows,
  filterTreeRows,
  joinTreePath,
  sortTreeEntries,
  toggleExpanded,
} from "./tree";

function entry(name: string, kind: TreeEntry["kind"] = "file"): TreeEntry {
  return { name, kind, size: kind === "file" ? 10 : null, oid: `oid-${name}` };
}

describe("joinTreePath", () => {
  it("joins onto an empty parent with no leading slash", () => {
    expect(joinTreePath("", "src")).toBe("src");
  });
  it("joins nested segments with a single slash", () => {
    expect(joinTreePath("src", "lib.rs")).toBe("src/lib.rs");
  });
});

describe("sortTreeEntries", () => {
  it("puts directories before files regardless of name", () => {
    const entries = [entry("zeta.rs"), entry("alpha", "dir")];
    const sorted = sortTreeEntries(entries);
    expect(sorted.map((e) => e.name)).toEqual(["alpha", "zeta.rs"]);
  });

  it("sorts each group case-insensitively", () => {
    const entries = [entry("Banana.rs"), entry("apple.rs"), entry("cherry.rs")];
    const sorted = sortTreeEntries(entries);
    expect(sorted.map((e) => e.name)).toEqual(["apple.rs", "Banana.rs", "cherry.rs"]);
  });

  it("does not mutate the input array", () => {
    const entries = [entry("b"), entry("a")];
    const copy = [...entries];
    sortTreeEntries(entries);
    expect(entries).toEqual(copy);
  });
});

describe("buildTreeRows", () => {
  it("flattens the root listing at depth 0", () => {
    const map = new Map([["", [entry("a.rs"), entry("src", "dir")]]]);
    const rows = buildTreeRows(map, new Set());
    expect(rows.map((r) => ({ path: r.path, depth: r.depth }))).toEqual([
      { path: "src", depth: 0 },
      { path: "a.rs", depth: 0 },
    ]);
  });

  it("descends into an expanded directory that has loaded entries", () => {
    const map = new Map([
      ["", [entry("src", "dir")]],
      ["src", [entry("lib.rs")]],
    ]);
    const rows = buildTreeRows(map, new Set(["src"]));
    expect(rows.map((r) => r.path)).toEqual(["src", "src/lib.rs"]);
    expect(rows[1].depth).toBe(1);
  });

  it("does not descend into a collapsed directory even if entries are loaded", () => {
    const map = new Map([
      ["", [entry("src", "dir")]],
      ["src", [entry("lib.rs")]],
    ]);
    const rows = buildTreeRows(map, new Set());
    expect(rows.map((r) => r.path)).toEqual(["src"]);
  });

  it("silently contributes no children for an expanded-but-not-yet-loaded directory", () => {
    const map = new Map([["", [entry("src", "dir")]]]);
    const rows = buildTreeRows(map, new Set(["src"]));
    expect(rows.map((r) => r.path)).toEqual(["src"]);
  });

  it("handles multiple levels of nesting", () => {
    const map = new Map([
      ["", [entry("a", "dir")]],
      ["a", [entry("b", "dir")]],
      ["a/b", [entry("c.rs")]],
    ]);
    const rows = buildTreeRows(map, new Set(["a", "a/b"]));
    expect(rows.map((r) => [r.path, r.depth])).toEqual([
      ["a", 0],
      ["a/b", 1],
      ["a/b/c.rs", 2],
    ]);
  });
});

describe("toggleExpanded", () => {
  it("adds an absent path", () => {
    const next = toggleExpanded(new Set(["a"]), "b");
    expect([...next].sort()).toEqual(["a", "b"]);
  });
  it("removes a present path", () => {
    const next = toggleExpanded(new Set(["a", "b"]), "b");
    expect([...next]).toEqual(["a"]);
  });
  it("never mutates the input set", () => {
    const original = new Set(["a"]);
    toggleExpanded(original, "b");
    expect(original.has("b")).toBe(false);
  });
});

describe("ancestorDirs", () => {
  it("returns every ancestor directory, deepest last", () => {
    expect(ancestorDirs("a/b/c.rs")).toEqual(["a", "a/b"]);
  });
  it("is empty for a root-level file", () => {
    expect(ancestorDirs("a.rs")).toEqual([]);
  });
});

describe("filterTreeRows", () => {
  const rows = buildTreeRows(
    new Map([["", [entry("src", "dir"), entry("README.md")]]]),
    new Set(),
  );

  it("returns every row for an empty query", () => {
    expect(filterTreeRows(rows, "")).toHaveLength(2);
  });

  it("matches case-insensitively against the full path", () => {
    expect(filterTreeRows(rows, "readme").map((r) => r.name)).toEqual(["README.md"]);
  });

  it("returns nothing for a query matching no row", () => {
    expect(filterTreeRows(rows, "nope")).toEqual([]);
  });
});
