// Pure data transforms for the left rail's virtualized file tree
// (`FileTree.tsx`). `GET /api/tree` is LAZY PER DIRECTORY (`TreeParams`'s
// doc in `crates/kb-code-server/src/routes.rs`) — the component holds one
// `useTree` query per directory it has ever expanded, keyed by path, and
// this module flattens that (directory → entries) map plus the caller's
// expanded-set into the single ordered row list `@tanstack/react-virtual`
// virtualizes over. Kept dependency-free (no React, no query client) so it
// vitest-covers without a DOM.

import type { EntryKind, TreeEntry } from "../api/types";
import { speedFilterItems } from "./speedSearch";

export interface TreeRow {
  /// Repo-relative path, forward-slash joined, no leading slash. `""` never
  /// appears as a row (the repo root itself isn't rendered as a row).
  path: string;
  name: string;
  kind: EntryKind;
  depth: number;
  size: number | null;
  oid: string;
}

export function joinTreePath(parent: string, name: string): string {
  return parent === "" ? name : `${parent}/${name}`;
}

/// Directories first, then files/symlinks/submodules, each group
/// case-insensitively alphabetical — the conventional file-tree sort every
/// mainstream editor uses.
export function sortTreeEntries(entries: TreeEntry[]): TreeEntry[] {
  const rank = (k: EntryKind): number => (k === "dir" ? 0 : 1);
  return [...entries].sort((a, b) => {
    const r = rank(a.kind) - rank(b.kind);
    if (r !== 0) return r;
    return a.name.localeCompare(b.name, undefined, { sensitivity: "base" });
  });
}

/// Flatten `entriesByPath` (one loaded directory listing per key, `""` =
/// repo root) into the ordered, depth-annotated row list for everything
/// currently visible under `expanded`. A directory that IS expanded but
/// has no entry in `entriesByPath` yet (its `useTree` query hasn't
/// resolved) simply contributes no child rows — the caller renders a
/// per-row loading affordance separately, keyed on the same path, rather
/// than this fn inventing a placeholder row.
export function buildTreeRows(
  entriesByPath: ReadonlyMap<string, TreeEntry[]>,
  expanded: ReadonlySet<string>,
): TreeRow[] {
  const rows: TreeRow[] = [];
  const walk = (dirPath: string, depth: number) => {
    const entries = entriesByPath.get(dirPath);
    if (!entries) return;
    for (const entry of sortTreeEntries(entries)) {
      const path = joinTreePath(dirPath, entry.name);
      rows.push({ path, name: entry.name, kind: entry.kind, depth, size: entry.size, oid: entry.oid });
      if (entry.kind === "dir" && expanded.has(path)) {
        walk(path, depth + 1);
      }
    }
  };
  walk("", 0);
  return rows;
}

/// Immutable toggle — returns a NEW Set with `path` added or removed,
/// never mutates `expanded` in place (React state setters need a new
/// reference to re-render).
export function toggleExpanded(expanded: ReadonlySet<string>, path: string): Set<string> {
  const next = new Set(expanded);
  if (next.has(path)) next.delete(path);
  else next.add(path);
  return next;
}

/// The chain of ancestor directory paths that must be expanded for `path`
/// to be visible (deepest-last) — used when a route/goto-file/quick-filter
/// jump lands on a file whose parents aren't expanded yet. `"a/b/c.rs"` →
/// `["a", "a/b"]` (the file itself is never included — files have nothing
/// to "expand").
export function ancestorDirs(path: string): string[] {
  const segments = path.split("/");
  segments.pop(); // drop the leaf (file or dir name itself)
  const out: string[] = [];
  let acc = "";
  for (const seg of segments) {
    acc = joinTreePath(acc, seg);
    out.push(acc);
  }
  return out;
}

/// Quick-filter over an already-flattened row list, matching on the FULL
/// path (so typing a directory prefix narrows too, not just the leaf name).
/// V3.N1: ranking rides the shared speed-search primitive (substring first,
/// subsequence fallback) — empty query is identity (stable order).
export function filterTreeRows(rows: TreeRow[], query: string): TreeRow[] {
  if (query.trim() === "") return rows;
  return speedFilterItems(rows, query, (r) => r.path).map((h) => h.item);
}
