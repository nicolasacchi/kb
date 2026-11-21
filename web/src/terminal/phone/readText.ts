// T17 — the Read view's text, taken from the panel's own xterm buffer.
//
// Walks `buffer.normal`, NOT `buffer.active`: in the alternate screen
// `active` has no scrollback, so a TUI would read as empty. Lines are the
// grid's hard-broken rows (never re-joined), right-trimmed.

import { findUrls } from "../links";

export interface ReadLine {
  translateToString(trimRight?: boolean): string;
  /** True when the row continues the previous one (xterm's soft wrap). */
  readonly isWrapped?: boolean;
}
export interface ReadBuffer {
  readonly length: number;
  getLine(y: number): ReadLine | undefined;
}

/** First code point of a kitty-graphics placeholder cell. */
export const PLACEHOLDER = "\u{10EEEE}";

/**
 * The rows, plus how many leading rows `max` cut off (a row index in the
 * buffer minus `dropped` is its index in `lines`).
 */
export function snapshotLines(buffer: ReadBuffer, opts: { max?: number } = {}): { lines: string[]; dropped: number } {
  const out: string[] = [];
  for (let y = 0; y < buffer.length; y += 1) {
    out.push(buffer.getLine(y)?.translateToString(true) ?? "");
  }
  while (out.length > 0 && out[out.length - 1] === "") out.pop();
  const max = opts.max;
  if (max !== undefined && max >= 0 && out.length > max) {
    return { lines: out.slice(out.length - max), dropped: out.length - max };
  }
  return { lines: out, dropped: 0 };
}

export function bufferToLines(buffer: ReadBuffer, opts: { max?: number } = {}): string[] {
  return snapshotLines(buffer, opts).lines;
}

/**
 * The text Copy all puts on the clipboard: soft-wrapped continuation rows are
 * joined back into the line the program printed (a wrapped URL copies whole),
 * keeping the cells at the wrap boundary; every other row keeps its own line.
 */
export function bufferToCopyText(buffer: ReadBuffer, opts: { max?: number } = {}): string {
  const logical: string[] = [];
  let cur = "";
  for (let y = 0; y < buffer.length; y += 1) {
    const row = buffer.getLine(y);
    const continues = buffer.getLine(y + 1)?.isWrapped === true;
    cur += row?.translateToString(!continues) ?? "";
    if (!continues) {
      logical.push(cur);
      cur = "";
    }
  }
  while (logical.length > 0 && logical[logical.length - 1] === "") logical.pop();
  const max = opts.max;
  const kept = max !== undefined && max >= 0 && logical.length > max ? logical.slice(logical.length - max) : logical;
  return kept.join("\n");
}

/** A line split into plain text and http(s) links (the same finder as the Watch grid). */
export function linkParts(line: string): Array<string | { url: string }> {
  if (!line.includes("http")) return [line];
  const out: Array<string | { url: string }> = [];
  let at = 0;
  for (const m of findUrls(line)) {
    if (m.start > at) out.push(line.slice(at, m.start));
    out.push({ url: m.url });
    at = m.end;
  }
  if (at < line.length) out.push(line.slice(at));
  return out;
}

/**
 * Scroll offset that puts the middle of line `line` at the middle of the view.
 * Lines are equal-height, so the line height is the content (minus `pad` top
 * and bottom) over the line count.
 */
export function centerScrollTop(a: {
  line: number;
  lines: number;
  contentHeight: number;
  viewHeight: number;
  pad: number;
}): number {
  if (a.lines <= 0) return 0;
  const lineH = (a.contentHeight - 2 * a.pad) / a.lines;
  const top = a.pad + (a.line + 0.5) * lineH - a.viewHeight / 2;
  return Math.max(0, Math.min(top, Math.max(0, a.contentHeight - a.viewHeight)));
}

/** A line made of placeholder cells reads as one dim `[image]` marker. */
export function isImageLine(line: string): boolean {
  return line.length > 0 && line.startsWith(PLACEHOLDER);
}
