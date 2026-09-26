// V76-C1 — the ONE snippet painter. Server `highlight/1` spans
// (`{line, start, end, role}`, UTF-8 byte columns) → the same `.kbc-hl-*`
// segments the reader already paints (`cssClassFor` in decorations.ts).
// No second class table.

import type { HighlightClass, HighlightSpan, Span } from "../api/types";
import {
  cssClassFor,
  lineIndexAt,
  lineStartByteOffsets,
  makeByteToUtf16Mapper,
  type ByteToUtf16Mapper,
} from "./decorations";
import {
  paintLine,
  splitContentLines,
  type LineSpan,
  type PaintedSegment,
} from "./diffHighlight";


/// Convert `GET /api/file` byte-offset spans onto the `highlight/1` wire
/// shape so LiveRefCard / file-backed cards share `paintSpans`.
export function byteSpansToHighlightSpans(content: string, spans: Span[]): HighlightSpan[] {
  if (content.length === 0 || spans.length === 0) return [];
  const { starts, totalBytes } = lineStartByteOffsets(content);
  const out: HighlightSpan[] = [];
  for (const span of spans) {
    if (span.byte_len <= 0) continue;
    const spanStart = span.byte_start;
    const spanEnd = span.byte_start + span.byte_len;
    if (spanEnd <= 0 || spanStart >= totalBytes) continue;
    const clippedStart = Math.max(0, spanStart);
    const clippedEnd = Math.min(totalBytes, spanEnd);
    if (clippedEnd <= clippedStart) continue;
    const firstLine = lineIndexAt(starts, clippedStart);
    const lastLine = lineIndexAt(starts, clippedEnd - 1);
    for (let li = firstLine; li <= lastLine; li++) {
      const lineByteStart = starts[li];
      const nextStart = starts[li + 1];
      const lineContentEnd = nextStart !== undefined ? nextStart - 1 : totalBytes;
      const overlapStart = Math.max(clippedStart, lineByteStart);
      const overlapEnd = Math.min(clippedEnd, lineContentEnd);
      if (overlapEnd <= overlapStart) continue;
      out.push({
        line: li + 1,
        start: overlapStart - lineByteStart,
        end: overlapEnd - lineByteStart,
        role: span.class,
      });
    }
  }
  return out;
}

/// Bucket highlight/1 spans onto 1-based lines as UTF-16 columns (the
/// shape `paintLine` already consumes).
///
/// A span's `start`/`end` are byte columns WITHIN its line, so the mapper
/// must be built over that one line — but once per LINE, not once per
/// span: `makeByteToUtf16Mapper` walks the whole line, so a file whose
/// every token is its own span would otherwise re-walk the same line
/// hundreds of times. One mapper per FILE is the rule for whole-file byte
/// offsets (`buildLineSpans`); one per LINE is the same rule one scope down.
export function wireSpansToLineMap(text: string, spans: HighlightSpan[]): Map<number, LineSpan[]> {
  const out = new Map<number, LineSpan[]>();
  if (text.length === 0 || spans.length === 0) return out;
  const lines = splitContentLines(text);
  // Lazily filled, so a snippet pays only for the lines it actually spans;
  // spans arrive in no guaranteed line order, hence the cache rather than a
  // pre-pass building a mapper for every line up front.
  const mappers = new Map<number, ByteToUtf16Mapper>();
  for (const s of spans) {
    const lineText = lines[s.line - 1];
    if (lineText === undefined || s.end <= s.start) continue;
    let mapper = mappers.get(s.line);
    if (mapper === undefined) {
      mapper = makeByteToUtf16Mapper(lineText);
      mappers.set(s.line, mapper);
    }
    const start = mapper(s.start);
    const end = mapper(s.end);
    if (end <= start) continue;
    const painted: LineSpan = { start, end, cls: cssClassFor(s.role as HighlightClass) };
    const bucket = out.get(s.line);
    if (bucket) bucket.push(painted);
    else out.set(s.line, [painted]);
  }
  return out;
}

/// Paint every line of `text`. Empty / missing spans yield one unclassed
/// segment per line — same DOM as the unhighlighted path.
export function paintSpans(text: string, spans: HighlightSpan[]): PaintedSegment[][] {
  const lines = splitContentLines(text);
  if (lines.length === 0) return [];
  const map = wireSpansToLineMap(text, spans);
  return lines.map((line, i) => paintLine(line, map.get(i + 1)));
}

/// Golden-friendly class sequence: `kbc-hl-keyword:fn` or `:plain`.
export function classSequence(text: string, spans: HighlightSpan[]): string[] {
  return paintSpans(text, spans).flatMap((segs) =>
    segs.map((s) => (s.cls ? `${s.cls}:${s.text}` : `:${s.text}`)),
  );
}
