// Pure data transforms for the left rail's virtualized file tree
// (`FileTree.tsx`). `GET /api/tree` is LAZY PER DIRECTORY (`TreeParams`'s
// doc in `crates/kb-code-server/src/routes.rs`) — the component holds one
// `useTree` query per directory it has ever expanded, keyed by path, and
// this module flattens that (directory → entries) map plus the caller's
// expanded-set into the single ordered row list `@tanstack/react-virtual`
// virtualizes over. Kept dependency-free (no React, no query client) so it
// vitest-covers without a DOM.

import type { EntryKind, TreeEntry, TreeRow as WireTreeRow } from "../api/types";

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

/// V71-F1 retired `filterTreeRows` (the tree's own `speedSearch.ts` call
/// site). Filtering the tree is now the DAEMON's job: `GET /api/tree/2`
/// ranks through the one nucleo matcher and returns match ranges, in
/// either of VS Code's two modes — so a client-side re-rank here would be
/// the second, differently-ranked matcher kbcq/1 exists to abolish
/// (kb-code-server/CLAUDE.md #16b). `speedSearch.ts` keeps its other list
/// filters; it just no longer has one here.

// ── V71-F1 — the legacy → kbc-tree/1 adapter ─────────────────────────────
//
// kbc-tree/1 is INDEX-only: it projects the mirror index, which is the
// working tree, not an arbitrary ref's ODB tree ("tree as of a ref" is F18
// and is deferred). So when the reader is browsing a NON-default ref, the
// dock keeps the pre-existing per-directory `GET /api/tree` listing — and
// converts it into the SAME `TreeRow` shape, so `FileTree` still has ONE
// render path and one keyboard model. Only the DATA SOURCE branches.
//
// A converted row carries no `facts`, no `trust` and no `match_ranges`, and
// says so by their absence — the decoration lanes are honestly unavailable
// at a ref, never rendered as zeroes.

export function legacyRowsToTreeRows(
  rows: readonly TreeRow[],
  expanded: ReadonlySet<string>,
): WireTreeRow[] {
  // Subtree file counts, computed from the rows actually loaded — a
  // directory whose listing has not arrived yet counts 0 and reports
  // `has_more`, rather than showing a number the SPA cannot back up.
  const fileCounts = new Map<string, number>();
  for (const r of rows) {
    if (r.kind !== "dir") {
      for (const dir of ancestorDirs(r.path)) {
        fileCounts.set(dir, (fileCounts.get(dir) ?? 0) + 1);
      }
    }
  }
  return rows.map((r) => ({
    id: r.kind === "dir" ? `d:${r.path}` : `f:${r.path}`,
    kind: r.kind === "dir" ? ("dir" as const) : ("file" as const),
    label: r.name,
    depth: r.depth,
    path: r.path,
    children: 0,
    files: r.kind === "dir" ? (fileCounts.get(r.path) ?? 0) : 1,
    has_more: r.kind === "dir" && !expanded.has(r.path),
  }));
}
