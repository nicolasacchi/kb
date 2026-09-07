// V74-L3c — group a recipe step's flat `KbcAddr[]` into a path tree for the
// `tree` result view. NOT a reuse of `components/FileTree.tsx`: that
// component drives its own live `GET /api/tree/2` fetch scoped/filtered
// server-side (`FileTreeProps`), whereas a recipe step's rows are already a
// bounded, server-picked set with no server-side notion of "this exact set,
// as a tree" to re-fetch — grouping the addresses we already have is the
// only shape that stays honest to what the step actually returned. Pure,
// so it is golden-testable without a DOM.

import type { KbcAddr } from "../api/types";

export interface RecipeTreeLeaf {
  kind: "leaf";
  name: string;
  addr: KbcAddr;
}

export interface RecipeTreeDir {
  kind: "dir";
  name: string;
  /// Full slash-joined path from the tree root, e.g. `"src/lib"` — stable
  /// React key material, never re-derived by walking parents at render time.
  path: string;
  children: RecipeTreeNode[];
}

export type RecipeTreeNode = RecipeTreeLeaf | RecipeTreeDir;

/// Rows with no `path` (an `entity`/`commit`/`fact` with no code anchor,
/// say) can't be placed on a path tree at all — returned separately so the
/// caller can render them as an honest "ungrouped" tail rather than
/// silently dropping them.
export interface RecipeTreeResult {
  roots: RecipeTreeNode[];
  ungrouped: KbcAddr[];
}

function findChildDir(children: RecipeTreeNode[], name: string): RecipeTreeDir | undefined {
  for (const c of children) {
    if (c.kind === "dir" && c.name === name) return c;
  }
  return undefined;
}

/// Build the tree. Directories sort before files, then alphabetically by
/// name (matches `components/FileTree.tsx`'s own convention) — deterministic
/// regardless of the input rows' order, so identical rows always render an
/// identical tree.
export function buildRecipeTree(rows: readonly KbcAddr[]): RecipeTreeResult {
  const roots: RecipeTreeNode[] = [];
  const ungrouped: KbcAddr[] = [];

  for (const addr of rows) {
    if (!addr.path) {
      ungrouped.push(addr);
      continue;
    }
    const segments = addr.path.split("/").filter((s) => s !== "");
    if (segments.length === 0) {
      ungrouped.push(addr);
      continue;
    }
    let level = roots;
    let prefix = "";
    for (let i = 0; i < segments.length - 1; i++) {
      const seg = segments[i];
      prefix = prefix ? `${prefix}/${seg}` : seg;
      let dir = findChildDir(level, seg);
      if (!dir) {
        dir = { kind: "dir", name: seg, path: prefix, children: [] };
        level.push(dir);
      }
      level = dir.children;
    }
    const leafName = segments[segments.length - 1];
    level.push({ kind: "leaf", name: leafName, addr });
  }

  sortTree(roots);
  return { roots, ungrouped };
}

function sortTree(nodes: RecipeTreeNode[]): void {
  nodes.sort((a, b) => {
    if (a.kind !== b.kind) return a.kind === "dir" ? -1 : 1;
    return a.name.localeCompare(b.name);
  });
  for (const n of nodes) {
    if (n.kind === "dir") sortTree(n.children);
  }
}
