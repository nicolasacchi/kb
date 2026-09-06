// Pure type-ahead filter for trees, rails, and recent-locations lists.
//
// Ranking: case-insensitive **substring first**, then **subsequence**
// fallback. Returns match ranges (UTF-16 offsets into the original
// haystack) so callers can highlight the matched characters without
// re-deriving the match. No React, no DOM — vitest-covered in
// `speedSearch.test.ts`.

export interface MatchRange {
  /** Inclusive start offset into the original (pre-lowercased) haystack. */
  start: number;
  /** Exclusive end offset. */
  end: number;
}

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

/// A single character of `text`, annotated with whether it lies inside a
/// match range — pure so components can map it to `<mark>` without
/// re-implementing the split.
export interface HighlightSegment {
  text: string;
  hit: boolean;
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

/// Split `text` into contiguous hit/miss segments for highlight rendering.
/// Ranges outside `[0, text.length)` are ignored; overlapping ranges are
/// coalesced by a simple mark-array walk (O(n + ranges)).
export function highlightSegments(text: string, ranges: readonly MatchRange[]): HighlightSegment[] {
  if (ranges.length === 0 || text.length === 0) {
    return text.length === 0 ? [] : [{ text, hit: false }];
  }
  const marks = new Array<boolean>(text.length).fill(false);
  for (const r of ranges) {
    const start = Math.max(0, Math.min(r.start, text.length));
    const end = Math.max(start, Math.min(r.end, text.length));
    for (let i = start; i < end; i++) marks[i] = true;
  }
  const out: HighlightSegment[] = [];
  let i = 0;
  while (i < text.length) {
    const hit = marks[i];
    let j = i + 1;
    while (j < text.length && marks[j] === hit) j++;
    out.push({ text: text.slice(i, j), hit });
    i = j;
  }
  return out;
}
