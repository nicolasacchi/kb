// T17 — the Read view's text, taken from the panel's own xterm buffer.
//
// Walks `buffer.normal`, NOT `buffer.active`: in the alternate screen
// `active` has no scrollback, so a TUI would read as empty. Lines are the
// grid's hard-broken rows (never re-joined), right-trimmed.

export interface ReadLine {
  translateToString(trimRight?: boolean): string;
}
export interface ReadBuffer {
  readonly length: number;
  getLine(y: number): ReadLine | undefined;
}

/** First code point of a kitty-graphics placeholder cell. */
export const PLACEHOLDER = "\u{10EEEE}";

export function bufferToLines(buffer: ReadBuffer, opts: { max?: number } = {}): string[] {
  const out: string[] = [];
  for (let y = 0; y < buffer.length; y += 1) {
    out.push(buffer.getLine(y)?.translateToString(true) ?? "");
  }
  while (out.length > 0 && out[out.length - 1] === "") out.pop();
  const max = opts.max;
  if (max !== undefined && max >= 0 && out.length > max) return out.slice(out.length - max);
  return out;
}

/** A line made of placeholder cells reads as one dim `[image]` marker. */
export function isImageLine(line: string): boolean {
  return line.length > 0 && line.startsWith(PLACEHOLDER);
}
