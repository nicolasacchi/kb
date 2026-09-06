// Pure derivations for the right rail's outline (`OutlineRail.tsx`) — the
// current file's `FileResponse.symbols` (`crates/kb-code-server/src/
// extract.rs`'s `Symbol`, or the YAML/TOML/JSON `kind = "key"` outline rows
// from `yaml.rs`/`keypath.rs`) turned into a click-to-scroll list.

import type { Symbol } from "../api/types";

export interface OutlineNode {
  symbol: Symbol;
  /// Nested symbols whose `container` names THIS node (Rust methods inside
  /// an `impl` block, nested YAML/TOML/JSON keys, …). Depth is implicit in
  /// nesting, not stored — `OutlineRail` renders recursively.
  children: OutlineNode[];
}

/// Group a flat, ordinal-ascending `Symbol[]` into a tree via each symbol's
/// `container` field (the nearest enclosing named construct — impl target
/// type, parent key path segment, …). A symbol whose `container` doesn't
/// match ANY other symbol's `name` in this file (the common case — most
/// symbols are top-level) becomes a root node. Symbols keep the server's
/// own emission order (`ordinal` ascending) both at the root and within
/// each parent's children — this fn does not re-sort.
///
/// Deliberately simple: matches on `container === name`, first hit wins
/// (mirrors `extract.rs`'s own `container_of` producing a single owning
/// name, not a qualified path) — a rare same-name-different-scope
/// collision folds under whichever candidate appears first, an accepted
/// v1 scope limit for a click-to-scroll aid, not a semantic index.
export function buildOutline(symbols: Symbol[]): OutlineNode[] {
  const byName = new Map<string, OutlineNode>();
  const nodes: OutlineNode[] = symbols.map((s) => {
    const node: OutlineNode = { symbol: s, children: [] };
    if (!byName.has(s.name)) byName.set(s.name, node);
    return node;
  });

  const roots: OutlineNode[] = [];
  for (const node of nodes) {
    const containerName = node.symbol.container;
    const parent = containerName ? byName.get(containerName) : undefined;
    if (parent && parent !== node) {
      parent.children.push(node);
    } else {
      roots.push(node);
    }
  }
  return roots;
}

/// Flatten an outline tree back into ordinal order, annotated with nesting
/// depth — what `OutlineRail` actually renders (a flat, indented list is
/// simpler to keyboard-navigate and virtualize than a real tree widget for
/// v1's typical file-sized symbol counts).
export function flattenOutline(nodes: OutlineNode[], depth = 0): { symbol: Symbol; depth: number }[] {
  const out: { symbol: Symbol; depth: number }[] = [];
  for (const node of nodes) {
    out.push({ symbol: node.symbol, depth });
    out.push(...flattenOutline(node.children, depth + 1));
  }
  return out;
}

/// Given the current scroll-derived line (or a click target), find the
/// symbol whose `[line_start, line_end]` range contains it, preferring the
/// SMALLEST (most specific) enclosing range — used to highlight the
/// active outline entry as the reader scrolls.
export function symbolAtLine(symbols: Symbol[], line: number): Symbol | null {
  let best: Symbol | null = null;
  for (const s of symbols) {
    if (line < s.line_start || line > s.line_end) continue;
    if (best === null || s.line_end - s.line_start < best.line_end - best.line_start) {
      best = s;
    }
  }
  return best;
}
