// Resolve a palette cursor position (or a mouse-clicked row) to WHAT
// SHOULD HAPPEN on Enter/click - pure, so `components/Omnibox.tsx` and
// `routes/Search.tsx` share exactly one lane-shaped resolution table
// instead of two independently-maintained switch statements (one for the
// keyboard Enter handler, one for each row's `onClick`).

import type {
  ChunkHit,
  FileHit,
  LaneSection,
  SessionHit,
  SymbolHit,
  TextFileResult,
  TranscriptHit,
} from "../api/types";
import { HEADER_ROW, type PaletteCursor } from "./paletteReducer";
import { extractRepoFilter, orderSections, sessionUrl, transcriptReaderPath } from "./searchLanes";
import { flattenTextRows } from "./searchRows";

export type SearchTarget =
  | { kind: "full-search" }
  | { kind: "reader"; repo: string; path: string; line?: number }
  | { kind: "external"; href: string }
  | { kind: "popover"; hit: TranscriptHit }
  | null;

/// `cursor.section` indexes the CANONICAL lane order (`orderSections`), the
/// same order every render pass produces - see `lib/searchLanes.ts`'s doc.
/// `fallbackRepo` is the route/omnibox's own repo scope (e.g. `/r/:repo`'s
/// param), used when a hit itself carries no `repo` (the text lane - see
/// `search::text`'s single-repo-per-request design) or when nothing
/// resolved from a `repo:` filter in `query`.
export function resolveSearchTarget(
  sections: LaneSection[],
  cursor: PaletteCursor,
  fallbackRepo: string | undefined,
  query: string,
): SearchTarget {
  const ordered = orderSections(sections);
  const section = ordered[cursor.section];
  if (!section) return null;
  if (cursor.row === HEADER_ROW) return { kind: "full-search" };

  const results = section.results;
  if (!Array.isArray(results)) return null;

  switch (section.lane) {
    case "files": {
      const hit = (results as FileHit[])[cursor.row];
      return hit ? { kind: "reader", repo: hit.repo, path: hit.path } : null;
    }
    case "symbols": {
      const hit = (results as SymbolHit[])[cursor.row];
      return hit ? { kind: "reader", repo: hit.repo, path: hit.path, line: hit.line_start } : null;
    }
    case "text": {
      const row = flattenTextRows(results as TextFileResult[])[cursor.row];
      if (!row) return null;
      const repo = extractRepoFilter(query) ?? fallbackRepo;
      return repo ? { kind: "reader", repo, path: row.path, line: row.line_no } : null;
    }
    case "semantic": {
      const hit = (results as ChunkHit[])[cursor.row];
      return hit ? { kind: "reader", repo: hit.repo, path: hit.path, line: hit.span_start } : null;
    }
    case "sessions": {
      const hit = (results as SessionHit[])[cursor.row];
      return hit ? { kind: "external", href: sessionUrl(hit.session_id) } : null;
    }
    case "transcripts": {
      const hit = (results as TranscriptHit[])[cursor.row];
      if (!hit) return null;
      const path = transcriptReaderPath(hit);
      return path ? { kind: "reader", repo: fallbackRepo ?? "", path } : { kind: "popover", hit };
    }
    default:
      return null;
  }
}
