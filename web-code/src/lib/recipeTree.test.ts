import { describe, expect, it } from "vitest";
import { buildRecipeTree } from "./recipeTree";
import type { KbcAddr } from "../api/types";

function addr(path?: string): KbcAddr {
  return { kind: "file", repo: "r", path, blob: "unknown", trust: "unknown" };
}

describe("buildRecipeTree", () => {
  it("empty input ⇒ empty tree, no ungrouped", () => {
    expect(buildRecipeTree([])).toEqual({ roots: [], ungrouped: [] });
  });

  it("groups nested paths into a directory tree", () => {
    const rows = [addr("src/lib/a.rs"), addr("src/lib/b.rs"), addr("src/main.rs"), addr("README.md")];
    const { roots, ungrouped } = buildRecipeTree(rows);
    expect(ungrouped).toEqual([]);
    // dirs before files, alphabetical: README.md (file), src (dir)
    expect(roots.map((r) => r.name)).toEqual(["src", "README.md"]);
    const src = roots[0];
    if (src.kind !== "dir") throw new Error("expected dir");
    expect(src.path).toBe("src");
    expect(src.children.map((c) => c.name)).toEqual(["lib", "main.rs"]);
    const lib = src.children[0];
    if (lib.kind !== "dir") throw new Error("expected dir");
    expect(lib.path).toBe("src/lib");
    expect(lib.children.map((c) => c.name)).toEqual(["a.rs", "b.rs"]);
  });

  it("routes a row with no path (or an empty path) to ungrouped, never dropped", () => {
    const rows = [addr(undefined), addr(""), addr("a.rs")];
    const { roots, ungrouped } = buildRecipeTree(rows);
    expect(roots.map((r) => r.name)).toEqual(["a.rs"]);
    expect(ungrouped).toHaveLength(2);
  });

  it("is deterministic regardless of input order", () => {
    const a = buildRecipeTree([addr("b/x.rs"), addr("a/y.rs"), addr("a.rs")]);
    const b = buildRecipeTree([addr("a.rs"), addr("a/y.rs"), addr("b/x.rs")]);
    expect(a).toEqual(b);
  });

  it("two files under the same directory share one dir node", () => {
    const { roots } = buildRecipeTree([addr("dir/a.rs"), addr("dir/b.rs")]);
    expect(roots).toHaveLength(1);
    const dir = roots[0];
    if (dir.kind !== "dir") throw new Error("expected dir");
    expect(dir.children).toHaveLength(2);
  });

  it("leaf carries the original Addr for downstream href/label resolution", () => {
    const one = addr("a.rs");
    const { roots } = buildRecipeTree([one]);
    const leaf = roots[0];
    if (leaf.kind !== "leaf") throw new Error("expected leaf");
    expect(leaf.addr).toBe(one);
  });
});
