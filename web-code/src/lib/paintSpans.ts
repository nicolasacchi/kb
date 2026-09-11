// V76-C1 — the ONE snippet painter. Server `highlight/1` spans
// (`{line, start, end, role}`, UTF-8 byte columns) → the same `.kbc-hl-*`
// segments the reader already paints (`cssClassFor` in decorations.ts).
// No second class table.

import type { HighlightClass, HighlightSpan, Span } from "../api/types";
import { cssClassFor, makeByteToUtf16Mapper } from "./decorations";
import {
  paintLine,
  splitContentLines,
  type LineSpan,
  type PaintedSegment,
} from "./diffHighlight";

function utf8ByteLength(codePoint: number): number {
  if (codePoint < 0x80) return 1;
  if (codePoint < 0x800) return 2;
  if (codePoint < 0x10000) return 3;
  return 4;
}

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
export function wireSpansToLineMap(text: string, spans: HighlightSpan[]): Map<number, LineSpan[]> {
  const out = new Map<number, LineSpan[]>();
  if (text.length === 0 || spans.length === 0) return out;
  const lines = splitContentLines(text);
  for (const s of spans) {
    const lineText = lines[s.line - 1];
    if (lineText === undefined || s.end <= s.start) continue;
    const mapper = makeByteToUtf16Mapper(lineText);
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

/// Shift snippet-relative 1-based lines onto a file line base (suggestion
/// blocks number from the comment's anchor, not from 1).
export function offsetHighlightSpans(spans: HighlightSpan[], lineBase: number): HighlightSpan[] {
  const delta = Math.max(1, lineBase) - 1;
  if (delta === 0) return spans;
  return spans.map((s) => ({ ...s, line: s.line + delta }));
}

/// Pad a snippet so `lines[n - 1]` is file line `n` (integrity guard).
export function padSnippetLines(text: string, lineBase: number): string[] {
  const body = splitContentLines(text);
  const pad = Math.max(0, lineBase - 1);
  if (pad === 0) return body;
  return [...Array<string>(pad).fill(""), ...body];
}
