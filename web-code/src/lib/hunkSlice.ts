// Pure helpers for the blame hover-diff "originating change" surface
// (V3.R2 / R12): given a fully-parsed unified diff and a target line, keep
// only the hunk(s) that cover that line on the NEW or OLD side, then cap
// the rendered line count so a hover card never dumps a multi-hundred-line
// patch. No fetch, no React — vitest-goldened.

import type { DiffHunk, DiffLine, ParsedDiff } from "./diff";

/// Default cap for the hover-diff body (brief: ~40 lines). Header lines
/// (hunk `@@` rows) count toward the budget so the card stays tight.
export const HOVER_DIFF_MAX_LINES = 40;

export interface HunkSliceOptions {
  /// 1-based line number in the NEW (post-image) file. Blame's
  /// `final_start` is this side.
  newLine?: number;
  /// 1-based line number in the OLD (pre-image) file. Optional — when
  /// both are set, a hunk matches if EITHER side covers its target.
  oldLine?: number;
  /// Max content lines (including hunk headers) to keep across all
  /// matching hunks. Defaults to {@link HOVER_DIFF_MAX_LINES}.
  maxLines?: number;
}

export interface HunkSliceResult {
  /// Matching hunks, possibly truncated mid-hunk when the budget is hit.
  hunks: DiffHunk[];
  /// `true` when at least one matching line was dropped by the cap, or a
  /// matching hunk was entirely omitted after the budget ran out.
  truncated: boolean;
  /// How many content lines were kept (headers + add/remove/context).
  keptLines: number;
}

function hunkCoversNew(hunk: DiffHunk, line: number): boolean {
  if (hunk.newLines <= 0) return false;
  return line >= hunk.newStart && line < hunk.newStart + hunk.newLines;
}

function hunkCoversOld(hunk: DiffHunk, line: number): boolean {
  if (hunk.oldLines <= 0) return false;
  return line >= hunk.oldStart && line < hunk.oldStart + hunk.oldLines;
}

function hunkMatches(hunk: DiffHunk, opts: HunkSliceOptions): boolean {
  if (opts.newLine !== undefined && hunkCoversNew(hunk, opts.newLine)) return true;
  if (opts.oldLine !== undefined && hunkCoversOld(hunk, opts.oldLine)) return true;
  return false;
}

/// Return only the hunks of `parsed` that cover the target line(s), capped
/// to `maxLines` rendered rows (each hunk header counts as one). Pure.
export function sliceHunksForLine(parsed: ParsedDiff, opts: HunkSliceOptions): HunkSliceResult {
  const maxLines = opts.maxLines ?? HOVER_DIFF_MAX_LINES;
  if (parsed.binary || parsed.hunks.length === 0) {
    return { hunks: [], truncated: false, keptLines: 0 };
  }
  if (opts.newLine === undefined && opts.oldLine === undefined) {
    return { hunks: [], truncated: false, keptLines: 0 };
  }

  const matching = parsed.hunks.filter((h) => hunkMatches(h, opts));
  if (matching.length === 0) {
    return { hunks: [], truncated: false, keptLines: 0 };
  }

  const out: DiffHunk[] = [];
  let kept = 0;
  let truncated = false;

  for (const hunk of matching) {
    // Budget already spent — remaining matching hunks are truncated away.
    if (kept >= maxLines) {
      truncated = true;
      break;
    }
    // Reserve one slot for the hunk header.
    if (kept + 1 > maxLines) {
      truncated = true;
      break;
    }
    kept += 1; // header
    const keptContent: DiffLine[] = [];
    for (const line of hunk.lines) {
      if (kept >= maxLines) {
        truncated = true;
        break;
      }
      keptContent.push(line);
      kept += 1;
    }
    if (keptContent.length < hunk.lines.length) truncated = true;
    out.push({
      header: hunk.header,
      oldStart: hunk.oldStart,
      oldLines: hunk.oldLines,
      newStart: hunk.newStart,
      newLines: hunk.newLines,
      lines: keptContent,
    });
  }

  return { hunks: out, truncated, keptLines: kept };
}

/// Convenience: rebuild a minimal {@link ParsedDiff} from a slice result
/// (for feeding `DiffHunks` without re-fetching).
export function slicedAsParsed(slice: HunkSliceResult): ParsedDiff {
  return {
    preamble: [],
    hunks: slice.hunks,
    binary: false,
  };
}
