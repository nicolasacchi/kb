// T14 — buffer find for the "Find" toggle.
//
// `@xterm/addon-search` is not in `web/package.json` (T1 shipped the
// other four xterm pins) and this unit never edits package files, so
// the native-input Find toggle searches the terminal buffer directly
// and highlights matches with `term.select`. Pure over a narrow line
// source, unit-testable without xterm.

export interface FindLineSource {
  /** Total buffer lines (`buffer.active.length`). */
  length: number;
  lineToString(y: number): string;
}

export interface FindHit {
  line: number;
  col: number;
  length: number;
}

/**
 * Find the next occurrence of `query` strictly after `from` (`dir = 1`)
 * or strictly before it (`dir = -1`), wrapping the whole buffer.
 * Returns null when there is no match or the query is empty.
 */
export function findNext(
  source: FindLineSource,
  query: string,
  from: FindHit,
  dir: 1 | -1,
): FindHit | null {
  if (query === "" || source.length === 0) return null;
  const n = source.length;
  for (let step = 0; step < n; step += 1) {
    const y = (((from.line + dir * step) % n) + n) % n;
    const text = source.lineToString(y);
    if (dir > 0) {
      const start = y === from.line ? from.col + 1 : 0;
      const idx = text.indexOf(query, start);
      if (idx >= 0) return { line: y, col: idx, length: query.length };
    } else {
      const start = y === from.line ? from.col - 1 : text.length - 1;
      if (start < 0) continue;
      const idx = text.lastIndexOf(query, Math.min(start, text.length - 1));
      if (idx >= 0) return { line: y, col: idx, length: query.length };
    }
  }
  return null;
}

/** Count occurrences of `query` across the whole buffer (Find caption). */
export function countHits(source: FindLineSource, query: string): number {
  if (query === "") return 0;
  let total = 0;
  for (let y = 0; y < source.length; y += 1) {
    let idx = source.lineToString(y).indexOf(query);
    while (idx >= 0) {
      total += 1;
      idx = source.lineToString(y).indexOf(query, idx + 1);
    }
  }
  return total;
}
