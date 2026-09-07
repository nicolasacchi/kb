// `kbc-pseudo/1` (V73-K2c) — the four reserved pseudo-file names, mirrored
// from `review_pseudo.rs`'s own consts. A pseudo path is never one of a
// review's real diffed files (`ReviewFileRow[]`), so every consumer that
// branches on "is this path a pseudo-file" (the map column's chapter zero,
// the diff center's single-file render, the timeline's `pr_body`/
// `doc_revision` links) shares this ONE prefix check rather than five
// separate `.startsWith("~review/")` calls drifting apart.
export const PSEUDO_PREFIX = "~review/";

export const PSEUDO_NAMES = ["pr-body.md", "review.md", "findings.json", "commits.md"] as const;
export type PseudoName = (typeof PSEUDO_NAMES)[number];

export function pseudoPath(name: PseudoName | string): string {
  return `${PSEUDO_PREFIX}${name}`;
}

export function isPseudoPath(path: string): boolean {
  return path.startsWith(PSEUDO_PREFIX);
}

/// `null` for anything not under the reserved prefix — total, never throws.
export function pseudoNameFromPath(path: string): string | null {
  return isPseudoPath(path) ? path.slice(PSEUDO_PREFIX.length) : null;
}
