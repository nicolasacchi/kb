// Match-range RENDERING (V71-D1). The half of the old `speedSearch.ts` that
// survives kbcq/1's "one matcher" rule.
//
// The MATCHING half — deciding which characters of a candidate a needle hit,
// and in what order candidates rank — now belongs to the daemon
// (`crates/kb-code-server/src/search/matcher.rs`, nucleo, one hard tier
// ladder), which ships the positions on the wire as UTF-16 `[start, end)`
// pairs. What stays client-side is the part that was never a matcher at all:
// turning positions into `<mark>`-able segments.
//
// Keeping these two apart is the point. `speedSearch.ts` re-exports from
// here, so its remaining list-filter consumers are untouched; nothing new
// should implement RANKING in TypeScript.

/** UTF-16 code-unit offsets into the string the range came with. */
export interface MatchRange {
  /** Inclusive start offset. */
  start: number;
  /** Exclusive end offset. */
  end: number;
}

/// A single run of `text`, annotated with whether it lies inside a match —
/// pure, so components map it to `<mark>` without re-implementing the split.
export interface HighlightSegment {
  text: string;
  hit: boolean;
}

/// The wire shape (`FileHit.ranges` / `SymbolHit.ranges`: `[start, end]`
/// pairs) → `MatchRange[]`. Total: an absent/short field is no highlight,
/// never a crash and never a guessed range.
export function fromWire(ranges: readonly (readonly number[])[] | undefined | null): MatchRange[] {
  if (!ranges) return [];
  const out: MatchRange[] = [];
  for (const r of ranges) {
    if (!r || r.length < 2) continue;
    const [start, end] = r;
    if (typeof start !== "number" || typeof end !== "number" || end <= start) continue;
    out.push({ start, end });
  }
  return out;
}

/// Re-base `ranges` onto the window `[offset, offset + length)` and drop
/// whatever falls outside it — for rendering ONE part of a composite
/// haystack (a symbol's `Container::name` is matched whole server-side, but
/// the row renders the name and the container in different elements).
/// Partially-overlapping ranges are CLIPPED, never dropped: half a match is
/// still true, an invented one is not.
export function sliceRanges(
  ranges: readonly MatchRange[],
  offset: number,
  length: number,
): MatchRange[] {
  const out: MatchRange[] = [];
  for (const r of ranges) {
    const start = Math.max(r.start, offset);
    const end = Math.min(r.end, offset + length);
    if (end > start) out.push({ start: start - offset, end: end - offset });
  }
  return out;
}

/// Split `text` into contiguous hit/miss segments for highlight rendering.
/// Ranges outside `[0, text.length)` are ignored; overlapping ranges are
/// coalesced by a simple mark-array walk (O(n + ranges)).
export function highlightSegments(
  text: string,
  ranges: readonly MatchRange[],
): HighlightSegment[] {
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
