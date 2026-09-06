// Pure type-ahead filter for trees, rails, and recent-locations lists.
//
// Ranking: case-insensitive **substring first**, then **subsequence**
// fallback. Returns match ranges (UTF-16 offsets into the original
// haystack) so callers can highlight the matched characters without
// re-deriving the match. No React, no DOM — vitest-covered in
// `speedSearch.test.ts`.
//
// ── DEPRECATED as a MATCHER (V71-D1, design D3) ────────────────────────
// kbcq/1's rule is ONE matcher: nucleo, server-side
// (`crates/kb-code-server/src/search/matcher.rs`), for every name-shaped
// lane and behind the tree/list filters — because two matchers means two
// rankings, and kb-code shipped exactly that (nucleo server-side with no
// match indices on the wire, this substring/subsequence ladder
// client-side WITH highlighting; recon/search.md §4 gap 17 and §8 q8).
// The server now returns its match indices, `matchRanges.ts` renders them,
// and the search lanes no longer come through here.
//
// What is still here, and why: this file's ~19 call sites are LIST filters
// (file tree, outline rail, structure popup, bookmark mnemonics, recent
// locations, ref typeahead, branch browser). Two of them have a server twin
// today (`GET /api/search/files`, `GET /api/search/symbols`) and the rest
// have none, so retiring the file wholesale in this unit would have meant
// either a new candidate-POST route — which the audit middleware would log
// as a mutation on every keystroke (kb-code-server/CLAUDE.md #1) — or
// nineteen conversions with no Playwright budget to verify them. The tree's
// own two-mode filter is V71-F1's stated scope and the results page is
// V71-D2's; they are what finish this.
//
// **Do not add ranking logic here.** New matching goes through the daemon.
// The range-RENDERING helpers below are re-exports from `matchRanges.ts`
// (never a matcher, and the one part with no server side).

export type { MatchRange, HighlightSegment } from "./matchRanges";
export { highlightSegments } from "./matchRanges";

import type { MatchRange } from "./matchRanges";

export type MatchKind = "substring" | "subsequence";

export interface SpeedMatch {
  ranges: MatchRange[];
  kind: MatchKind;
  /** Lower is better. Used only for ordering among hits. */
  rank: number;
}

export interface SpeedFilterHit<T> {
  item: T;
  ranges: MatchRange[];
  kind: MatchKind;
  rank: number;
}

/// Match `query` against `haystack`. Empty/whitespace-only query matches
/// everything with no ranges (identity filter). Non-ASCII in the haystack
/// is just another character — matching is code-unit based after a single
/// `toLowerCase()` pass (no locale folding).
export function matchSpeedSearch(haystack: string, query: string): SpeedMatch | null {
  const q = query.trim();
  if (q === "") return { ranges: [], kind: "substring", rank: 0 };

  const hLower = haystack.toLowerCase();
  const qLower = q.toLowerCase();

  // --- substring (preferred) --------------------------------------------
  const subIdx = hLower.indexOf(qLower);
  if (subIdx >= 0) {
    // rank: earlier match + shorter haystack wins among substring hits.
    const rank = subIdx * 1000 + haystack.length;
    return {
      ranges: [{ start: subIdx, end: subIdx + qLower.length }],
      kind: "substring",
      rank,
    };
  }

  // --- subsequence fallback ---------------------------------------------
  const ranges: MatchRange[] = [];
  let from = 0;
  for (let qi = 0; qi < qLower.length; qi++) {
    const ch = qLower[qi];
    const i = hLower.indexOf(ch, from);
    if (i < 0) return null;
    if (ranges.length > 0 && ranges[ranges.length - 1].end === i) {
      ranges[ranges.length - 1].end = i + 1;
    } else {
      ranges.push({ start: i, end: i + 1 });
    }
    from = i + 1;
  }
  // Subsequence ranks AFTER every substring hit (offset by 1e9) so a
  // caller sorting by `rank` ascending gets the brief's "substring-first"
  // order for free.
  const first = ranges[0]?.start ?? 0;
  const rank = 1_000_000_000 + first * 1000 + haystack.length;
  return { ranges, kind: "subsequence", rank };
}

/// Filter + rank a list. Empty query returns every item (stable order,
/// empty ranges). Non-empty query drops non-matches and sorts by `rank`.
export function speedFilterItems<T>(
  items: readonly T[],
  query: string,
  textOf: (item: T) => string,
): SpeedFilterHit<T>[] {
  const q = query.trim();
  if (q === "") {
    return items.map((item) => ({ item, ranges: [], kind: "substring" as const, rank: 0 }));
  }
  const hits: SpeedFilterHit<T>[] = [];
  for (const item of items) {
    const m = matchSpeedSearch(textOf(item), q);
    if (m) hits.push({ item, ranges: m.ranges, kind: m.kind, rank: m.rank });
  }
  hits.sort((a, b) => a.rank - b.rank);
  return hits;
}
