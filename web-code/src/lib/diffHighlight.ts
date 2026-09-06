// Bucket server highlight spans (`FileResponse.highlights`, UTF-8 byte
// offsets) onto 1-based diff lines as UTF-16 columns, then paint a line
// into classed segments. Pure — no DOM, no React. The hook
// (`useDiffHighlights`) builds one map per side; UnifiedHunks/SplitHunks
// paint only when the per-line integrity guard (`DiffLine.text ===` the
// file's line at that number) passes.

import type { HighlightClass, Span } from "../api/types";
import { cssClassFor, makeByteToUtf16Mapper } from "./decorations";

/** Server span — same shape as `api/types.Span`. */
export type ServerSpan = Span;

/** UTF-16 columns within one line. `cls` is the `.kbc-hl-*` class. */
export interface LineSpan {
  start: number;
  end: number;
  cls: string;
}

export interface DiffHighlights {
  oldLineSpans: Map<number, LineSpan[]>;
  newLineSpans: Map<number, LineSpan[]>;
  /// 0-indexed file lines for the integrity guard (`lines[n-1]` is
  /// 1-based line `n`). `null` when that side was not fetched or degraded.
  oldLines: string[] | null;
  newLines: string[] | null;
}

export interface PaintedSegment {
  text: string;
  cls?: string;
}

function utf8ByteLength(codePoint: number): number {
  if (codePoint < 0x80) return 1;
  if (codePoint < 0x800) return 2;
  if (codePoint < 0x10000) return 3;
  return 4;
}

/// Split file content into lines the same way `parseUnifiedDiff` does
/// (drop a single trailing empty element from a final `"\n"`). CRLF is
/// NOT stripped — a `\r` left on the line fails the integrity guard,
/// which is the point (wrong-eol is unsafe to paint).
export function splitContentLines(content: string): string[] {
  const lines = content.split("\n");
  if (lines.length > 0 && lines[lines.length - 1] === "") lines.pop();
  return lines;
}

/// 0-indexed: `starts[i]` is the UTF-8 byte offset of 1-based line `i+1`.
function lineStartByteOffsets(content: string): { starts: number[]; totalBytes: number } {
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

/// Largest index whose start is `<= byte`.
function lineIndexAt(starts: number[], byte: number): number {
  let lo = 0;
  let hi = starts.length - 1;
  while (lo < hi) {
    const mid = (lo + hi + 1) >> 1;
    if (starts[mid] <= byte) lo = mid;
    else hi = mid - 1;
  }
  return lo;
}

/// One pass over line-start byte offsets; each server span is bucketed
/// into the line(s) it overlaps and converted to UTF-16 columns via
/// `makeByteToUtf16Mapper` on the whole file. Multi-line spans split at
/// line boundaries (the newline itself is never a column).
export function buildLineSpans(content: string, spans: ServerSpan[]): Map<number, LineSpan[]> {
  const out = new Map<number, LineSpan[]>();
  if (content.length === 0 || spans.length === 0) return out;

  const { starts, totalBytes } = lineStartByteOffsets(content);
  const mapper = makeByteToUtf16Mapper(content);

  for (const span of spans) {
    if (span.byte_len <= 0) continue;
    const spanStart = span.byte_start;
    const spanEnd = span.byte_start + span.byte_len;
    if (spanEnd <= 0 || spanStart >= totalBytes) continue;

    const clippedStart = Math.max(0, spanStart);
    const clippedEnd = Math.min(totalBytes, spanEnd);
    if (clippedEnd <= clippedStart) continue;

    const firstLine = lineIndexAt(starts, clippedStart);
    // Exclusive end on a line-start belongs to the previous line.
    const lastLine = lineIndexAt(starts, clippedEnd - 1);
    const cls = cssClassFor(span.class as HighlightClass);

    for (let li = firstLine; li <= lastLine; li++) {
      const lineByteStart = starts[li];
      const nextStart = starts[li + 1];
      // Line text excludes the terminating `\n` (always 1 UTF-8 byte).
      const lineContentEnd = nextStart !== undefined ? nextStart - 1 : totalBytes;
      const overlapStart = Math.max(clippedStart, lineByteStart);
      const overlapEnd = Math.min(clippedEnd, lineContentEnd);
      if (overlapEnd <= overlapStart) continue;

      const lineUtf16 = mapper(lineByteStart);
      const start = mapper(overlapStart) - lineUtf16;
      const end = mapper(overlapEnd) - lineUtf16;
      if (end <= start) continue;

      const lineNo = li + 1;
      const bucket = out.get(lineNo);
      const painted: LineSpan = { start, end, cls };
      if (bucket) bucket.push(painted);
      else out.set(lineNo, [painted]);
    }
  }
  return out;
}

/// Split `text` into contiguous classed segments. Empty / missing spans
/// yield a single unclassed segment (callers render that as plain text —
/// same DOM as the unhighlighted path).
///
/// Overlapping spans: last-wins. The server (`highlight::extract_highlights`)
/// already guarantees a non-overlapping sequence; this is defensive. Later
/// entries in the `LineSpan` array overwrite earlier coverage on shared
/// UTF-16 columns (paint-time, not clip-at-build).
export function paintLine(text: string, spans: LineSpan[] | undefined): PaintedSegment[] {
  if (!spans || spans.length === 0 || text.length === 0) {
    return [{ text }];
  }

  const cover: (string | undefined)[] = new Array(text.length);
  for (const span of spans) {
    const start = Math.max(0, span.start);
    const end = Math.min(text.length, span.end);
    if (end <= start || !span.cls) continue;
    for (let i = start; i < end; i++) cover[i] = span.cls;
  }

  const segs: PaintedSegment[] = [];
  let i = 0;
  while (i < text.length) {
    const cls = cover[i];
    let j = i + 1;
    while (j < text.length && cover[j] === cls) j += 1;
    segs.push(cls ? { text: text.slice(i, j), cls } : { text: text.slice(i, j) });
    i = j;
  }
  return segs.length > 0 ? segs : [{ text }];
}

/// Integrity-guarded lookup. Returns `undefined` spans when the file line
/// at `n` is missing or does not byte-equal `text` — callers then paint
/// plain. `side` picks old vs new maps.
export function spansForLine(
  highlights: DiffHighlights | null | undefined,
  side: "old" | "new",
  n: number | null,
  text: string,
): LineSpan[] | undefined {
  if (!highlights || n === null) return undefined;
  const lines = side === "old" ? highlights.oldLines : highlights.newLines;
  if (!lines) return undefined;
  if (lines[n - 1] !== text) return undefined;
  const map = side === "old" ? highlights.oldLineSpans : highlights.newLineSpans;
  return map.get(n);
}
