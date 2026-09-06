// V70-A3S — Home's cross-repo "Continue where you left off" card (kb-code
// v7 design doc §Decisions D23's "continue where you left off" card). Pure
// derivation over `lib/navHistory.ts`'s own `NavLocation` ring — a SEPARATE
// helper from that module's `recentFilesFrom` (not an edit to it): that
// function's pinned golden (`navHistory.test.ts`) deliberately drops
// `line` from its output (`{repo, path, ts}` only), but Home's card needs
// "path:line" for each entry (unlike the per-repo `ReaderStartCards` in
// Reader.tsx, which only ever renders a bare path). This keeps the EXACT
// same dedup algorithm `recentFilesFrom` uses (unique by (repo,path),
// newest first) while retaining the line the operator was last at.

import type { NavLocation, RecentFile } from "./navHistory";

export interface ContinueLocation extends RecentFile {
  line: number;
}

export const CONTINUE_LOCATIONS_CAP = 5;

/// Unique-by-(repo,path) view of the location ring, newest first, capped —
/// same algorithm as `navHistory.ts`'s `recentFilesFrom`, plus `line`.
export function continueLocations(
  entries: readonly NavLocation[],
  cap: number = CONTINUE_LOCATIONS_CAP,
): ContinueLocation[] {
  const out: ContinueLocation[] = [];
  const seen = new Set<string>();
  for (const e of entries) {
    const key = `${e.repo}\0${e.path}`;
    if (seen.has(key)) continue;
    seen.add(key);
    out.push({ repo: e.repo, path: e.path, ts: e.ts, line: e.line });
    if (out.length >= cap) break;
  }
  return out;
}
