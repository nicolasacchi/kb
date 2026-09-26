// Server highlight spans (`Span.byte_start`/`byte_len`,
// `crates/kb-code-server/src/highlight.rs`) and symbol line/col positions
// are UTF-8 BYTE offsets (tree-sitter's own convention — see that module's
// doc). CodeMirror 6 positions are UTF-16 code-unit offsets into the JS
// string it was handed (`FileResponse.content`). For pure-ASCII source
// (the overwhelming common case in the six-plus languages kb-code indexes)
// the two coincide 1:1; a mapper is only needed once a file contains
// multi-byte UTF-8 (a non-ASCII string literal, a comment, an emoji, …).
//
// `CodeView` builds one mapper per file load (`useMemo`, keyed on
// `blob_hash`) and reuses it for every span — NOT one mapper per span,
// which would be O(n) per lookup instead of one O(n) precompute + O(log n)
// per lookup.

import type { HighlightClass, Span } from "../api/types";

export type ByteToUtf16Mapper = (byteOffset: number) => number;

/// UTF-8 byte width of ONE Unicode scalar value. The single definition for
/// the whole codebase (the `makeByteToUtf16Mapper` walk, the per-line
/// offsets below and `utf8LengthOf` all funnel through it) — a second copy
/// is a second thing to keep correct, and these are exactly the helpers
/// where a wrong bound shows up as silently mis-coloured code.
export function utf8ByteLength(codePoint: number): number {
  if (codePoint < 0x80) return 1;
  if (codePoint < 0x800) return 2;
  if (codePoint < 0x10000) return 3;
  return 4;
}

/// EXACT UTF-8 byte length of `content`, in one pass and with no
/// allocation. `new TextEncoder().encode(text).length` materialises a whole
/// byte-array copy of the string (up to 1.5 MiB per diff side) purely to
/// learn a number; this counts in place. The bytes MUST be exact, never
/// the UTF-16 `.length` — the server measures Rust `str::len()`.
///
/// `cap` is a short-circuit for that same size check. A string's UTF-8 byte
/// length is never LESS than its UTF-16 `.length` (every BMP scalar costs
/// ≥ 1 byte, an astral one 4 bytes for 2 units), so `content.length > cap`
/// already PROVES the answer is over the cap — which is the common
/// ASCII/large case, settled in one comparison instead of a walk. It
/// returns `cap + 1`, deliberately not `cap`, so a caller's `<= cap` test is
/// still false. Anything not short-circuited gets the exact walk, and the
/// default cap makes the one-argument call exact for every input.
export function utf8LengthOf(content: string, cap = Number.POSITIVE_INFINITY): number {
  if (content.length > cap) return cap + 1;
  let bytes = 0;
  const n = content.length;
  let i = 0;
  while (i < n) {
    const code = content.codePointAt(i) as number;
    bytes += utf8ByteLength(code);
    i += code > 0xffff ? 2 : 1;
  }
  return bytes;
}

/// 0-indexed: `starts[i]` is the UTF-8 byte offset of 1-based line `i+1`
/// (and `totalBytes` is the whole string's length). Shared by both
/// byte→line painters — `buildLineSpans` for a `GET /api/file` side and
/// `byteSpansToHighlightSpans` for a `highlight/1` snippet — because the
/// two must agree on where a line begins or a span lands on the wrong line.
export function lineStartByteOffsets(content: string): {
  starts: number[];
  totalBytes: number;
} {
  const starts = [0];
  let byteOffset = 0;
  let i = 0;
  const n = content.length;
  while (i < n) {
    const code = content.codePointAt(i) as number;
    const utf16Len = code > 0xffff ? 2 : 1;
    byteOffset += utf8ByteLength(code);
    i += utf16Len;
    if (code === 10) starts.push(byteOffset);
  }
  return { starts, totalBytes: byteOffset };
}

/// Largest index whose start is `<= byte` — the line a span STARTS on.
/// Binary search, not a scan: a whole-file sweep of `starts` per span would
/// make painting O(spans × lines), and a file can carry both tens of
/// thousands.
export function lineIndexAt(starts: number[], byte: number): number {
  let lo = 0;
  let hi = starts.length - 1;
  while (lo < hi) {
    const mid = (lo + hi + 1) >> 1;
    if (starts[mid] <= byte) lo = mid;
    else hi = mid - 1;
  }
  return lo;
}

/// Build a byte-offset → UTF-16-index mapper for `content`. Single forward
/// pass over the string (one `codePointAt` per Unicode scalar value, not
/// per UTF-16 code unit — a surrogate pair is visited once). The returned
/// closure binary-searches the precomputed checkpoint arrays, so repeated
/// lookups (one per highlight span / symbol position in the file) are
/// O(log n) each rather than re-walking the string every time.
export function makeByteToUtf16Mapper(content: string): ByteToUtf16Mapper {
  // Parallel checkpoint arrays, index k = "after the k-th Unicode scalar
  // value": `byteCheckpoints[k]` is the cumulative UTF-8 byte length up to
  // there, `utf16Checkpoints[k]` the cumulative UTF-16 code-unit length.
  // Both start at [0] (the empty prefix) so `target <= 0` and an exact hit
  // on the first codepoint both resolve correctly via the search below.
  const byteCheckpoints: number[] = [0];
  const utf16Checkpoints: number[] = [0];
  let byteOffset = 0;
  let utf16Offset = 0;
  const n = content.length;
  let i = 0;
  while (i < n) {
    const code = content.codePointAt(i) as number;
    const utf16Len = code > 0xffff ? 2 : 1;
    byteOffset += utf8ByteLength(code);
    utf16Offset += utf16Len;
    i += utf16Len;
    byteCheckpoints.push(byteOffset);
    utf16Checkpoints.push(utf16Offset);
  }

  return (target: number): number => {
    if (target <= 0) return 0;
    if (target >= byteOffset) return utf16Offset;
    // Largest checkpoint index whose byte offset is <= target.
    let lo = 0;
    let hi = byteCheckpoints.length - 1;
    while (lo < hi) {
      const mid = (lo + hi + 1) >> 1;
      if (byteCheckpoints[mid] <= target) lo = mid;
      else hi = mid - 1;
    }
    return utf16Checkpoints[lo];
  };
}

export interface DecorationRange {
  from: number;
  to: number;
  class: HighlightClass;
}

/// One CSS class per `HighlightClass` bucket — mirrors
/// `highlight::HighlightClass`'s fixed EIGHTEEN-member set 1:1 (V72-H2b,
/// D16; `styles/reader.css` defines each `.kbc-hl-*` rule and
/// `styles/tokens.css` the `--syn-*` role behind it). The wire values are
/// kebab-case, so a multi-word role is `kbc-hl-string-special` — no
/// translation happens here, which is why widening the server enum needed
/// no change to this function.
export function cssClassFor(cls: HighlightClass): string {
  return `kbc-hl-${cls}`;
}

/// Map server byte-offset spans onto UTF-16 `[from, to)` ranges CodeMirror
/// can turn into `Decoration.mark` ranges. The server's own sweep
/// (`highlight::extract_highlights`) already guarantees a non-overlapping,
/// in-bounds, `byte_start`-ascending sequence — this fn trusts that
/// ordering (does not re-sort) but still drops any range that becomes
/// empty after byte→UTF-16 mapping (can't happen for well-formed spans,
/// but a zero-width decoration would throw inside CodeMirror's
/// `RangeSetBuilder`, so this is a real defensive check, not paranoia) and
/// clamps `to` to the mapper's own end so a corrupt/truncated span can
/// never produce an out-of-bounds decoration.
export function spansToDecorationRanges(
  content: string,
  spans: Span[],
  mapper: ByteToUtf16Mapper = makeByteToUtf16Mapper(content),
): DecorationRange[] {
  const maxUtf16 = content.length;
  const ranges: DecorationRange[] = [];
  for (const span of spans) {
    const from = mapper(span.byte_start);
    const to = Math.min(mapper(span.byte_start + span.byte_len), maxUtf16);
    if (to <= from) continue;
    ranges.push({ from, to, class: span.class });
  }
  return ranges;
}
