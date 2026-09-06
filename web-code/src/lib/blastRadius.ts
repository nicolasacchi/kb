// Pure helpers for R8 blast-radius strip (V3.1-H3b): pick top ≤3 symbols
// whose declaration spans overlap changed hunk lines in a review diff.

import type { Symbol } from "../api/types";
import type { ParsedDiff } from "./diff";

/** Collect 1-based new-file line numbers that were added or context-near
 *  changed regions (added lines + surrounding context lines that sit in a
 *  hunk). Uses the post-image (`newLine`) so we match tip symbols. */
export function changedNewLines(parsed: ParsedDiff): Set<number> {
  const out = new Set<number>();
  for (const h of parsed.hunks) {
    for (const line of h.lines) {
      if (line.kind === "add" && line.newLine != null) out.add(line.newLine);
      // Include the first context line after a change block? Prefer only
      // true adds — symbols that only appear as context are not "changed".
    }
  }
  return out;
}

/**
 * Symbols whose `[line_start, line_end]` intersects any changed new-line.
 * Sorted by line_start; capped at `limit` (default 3).
 */
export function symbolsInChangedHunks(
  symbols: Symbol[],
  changedLines: Set<number>,
  limit = 3,
): Symbol[] {
  if (changedLines.size === 0 || symbols.length === 0) return [];
  const hits: Symbol[] = [];
  for (const s of symbols) {
    // Declaration-ish kinds preferred; still accept any covering symbol.
    let hit = false;
    for (let ln = s.line_start; ln <= s.line_end; ln++) {
      if (changedLines.has(ln)) {
        hit = true;
        break;
      }
    }
    // Also: symbol declared exactly on a changed line (even if span is 1).
    if (!hit && changedLines.has(s.line_start)) hit = true;
    if (hit) hits.push(s);
  }
  hits.sort((a, b) => a.line_start - b.line_start || a.ordinal - b.ordinal);
  // Prefer outer-most / declaration kinds when over limit: keep first N by line.
  return hits.slice(0, limit);
}
