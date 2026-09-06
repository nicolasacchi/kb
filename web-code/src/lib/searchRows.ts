// Per-lane row shaping for the Search-Everywhere box (W4.3) - pure
// derivations shared by `components/search/SearchSection.tsx` (rendering)
// and `lib/searchTargets.ts` (click/Enter resolution), so both read the
// SAME flattened row list rather than two independently-maintained loops.

import type { TextFileResult, TextMatch } from "../api/types";
import { makeByteToUtf16Mapper } from "./decorations";

/// One navigable row in the text lane. `search::text::TextFileResult` is
/// one row per FILE with an array of matches (`search::text`'s own
/// "line-oriented, one SinkMatch per matching line" convention); the
/// omnibox/search page render one row PER MATCH (the box's own "path +
/// line + highlighted match line" row shape), so this flattens the
/// server's per-file grouping into the box's per-match row list. Order is
/// preserved: file order, then match order within each file (both already
/// server-ranked).
export interface TextRow {
  path: string;
  line_no: number;
  line: string;
  byte_range: [number, number];
}

export function flattenTextRows(results: TextFileResult[]): TextRow[] {
  const rows: TextRow[] = [];
  for (const file of results) {
    for (const m of file.matches) {
      rows.push({ path: file.path, line_no: m.line_no, line: m.line, byte_range: m.byte_range });
    }
  }
  return rows;
}

export interface HighlightedLine {
  before: string;
  match: string;
  after: string;
}

/// Split a text-lane match's `line` into pre-match/match/post-match slices
/// for highlighting. `byteRange` is UTF-8 BYTE offsets WITHIN `line`
/// (`grep_matcher`'s convention - see `TextMatch`'s doc in `api/types.ts`),
/// so this reuses the SAME byte->UTF-16 mapper `CodeView` uses for syntax
/// highlight spans (`lib/decorations.ts`) rather than assuming ASCII.
export function highlightMatch(line: string, byteRange: TextMatch["byte_range"]): HighlightedLine {
  const mapper = makeByteToUtf16Mapper(line);
  const from = mapper(byteRange[0]);
  const to = Math.min(mapper(byteRange[1]), line.length);
  if (to <= from) return { before: line, match: "", after: "" };
  return { before: line.slice(0, from), match: line.slice(from, to), after: line.slice(to) };
}

/// `SessionHit.started_at` is unix SECONDS (kb's own session-digest
/// convention). A plain `YYYY-MM-DD` - deterministic under the vitest
/// harness's pinned `TZ=UTC` (`vitest.config.ts`) and locale-independent,
/// unlike `toLocaleDateString()`.
export function formatSessionDate(unixSeconds: number): string {
  return new Date(unixSeconds * 1000).toISOString().slice(0, 10);
}
