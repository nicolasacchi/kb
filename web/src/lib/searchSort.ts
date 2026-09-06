import type { SearchSort } from "../api/client";

export type { SearchSort } from "../api/client";
export type SortDir = "asc" | "desc";

// FS4 — the search page's sort menu. A superset of the gallery's
// lib/sort SortKey: adds `relevance` (the default — the score order the
// daemon already returns) plus the two reading-rollup axes (`opened`,
// `progress`). Unlike the gallery (which re-orders ≤200 client rows),
// search sorting is SERVER-SIDE — the daemon re-orders the matched pool
// before truncation — so this module only labels the keys and picks
// default directions for the URL.

export const SEARCH_SORTS: { key: SearchSort; label: string }[] = [
  { key: "relevance", label: "relevance" },
  { key: "opened", label: "last opened" },
  { key: "modified", label: "modified" },
  { key: "created", label: "created" },
  { key: "indexed", label: "indexed" },
  { key: "title", label: "title A→Z" },
  { key: "words", label: "word count" },
  { key: "progress", label: "reading progress" },
];

// Default direction per key: title ascends (A→Z); everything else
// descends (newest / most / furthest-read first). `relevance` has no
// meaningful direction (it's the fixed score order).
export function defaultSearchDir(key: SearchSort): SortDir {
  return key === "title" ? "asc" : "desc";
}
