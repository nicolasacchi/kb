// V80-M1 — "All files": the tip sha's WHOLE tree, for the review diff's
// file map/tree mode toggle (`?files=all`, `web-code/CLAUDE.md`'s Review
// diff v2 section). `GET /api/tree` (the reader's own per-directory ODB
// listing, `hooks/useTree.ts`) is per-directory and non-recursive by design
// (root CLAUDE.md's "frozen" `/tree` — kbc-tree/1's `/tree/2` is a DIFFERENT
// projection, not a flat file list); `lib/reviewFileTree.ts`'s `walkAllFiles`
// walks it recursively, client-side, the same way `components/FileTree.tsx`
// lazily expands one directory at a time — just eagerly, since the tree
// renderer here needs the whole list up front rather than expand-on-click.
// This module is the thin `fetchTree` wiring; the walk itself is pure
// (injectable `listDir`) and unit-pinned in `lib/reviewFileTree.test.ts`.

import { useQuery } from "@tanstack/react-query";
import { fetchTree } from "../api/client";
import { ALL_FILES_CAP, walkAllFiles, type AllFilesResult } from "../lib/reviewFileTree";

export type { AllFilesResult };
export { ALL_FILES_CAP };

async function fetchAllFiles(repo: string, ref: string): Promise<AllFilesResult> {
  return walkAllFiles((dir) => fetchTree(repo, dir, ref).then((r) => r.entries), ALL_FILES_CAP);
}

/// `enabled` — callers gate this on `filesMode === "all"`: walking the
/// whole tree is wasted work (and, for a large repo, a real cost) the
/// default "changed files" view never needs.
export function useReviewAllFiles(
  repo: string | undefined,
  ref: string | undefined,
  enabled: boolean,
) {
  return useQuery({
    queryKey: ["review-all-files", repo, ref ?? null],
    queryFn: () => fetchAllFiles(repo as string, ref as string),
    enabled: enabled && repo !== undefined && ref !== undefined,
    staleTime: 60_000,
  });
}
