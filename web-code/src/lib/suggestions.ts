// Pure helpers for V4.S2 in-thread suggestion editor + apply chrome.
// The server owns capture (`original` + `base_blob_sha`) and the exact-match
// splice; this file only synthesizes a one-hunk preview and the one
// client-derivable outdated signal.

import type { ReviewComment } from "../api/types";
import type { DiffLine, ParsedDiff } from "./diff";
import { splitContentLines } from "./diffHighlight";

/// Split stored suggestion text the same way the server joins it
/// (`line..=line_end` with `\n`). An empty string is zero lines, not one
/// empty line — so a pure-deletion replacement and an empty original both
/// round-trip as `[]`.
export function splitSuggestionLines(text: string): string[] {
  return splitContentLines(text);
}

/// Slice `startLine..=endLine` (1-based, inclusive; `endLine` defaults to
/// `startLine`) out of tip-blob content. Out-of-range lines are dropped.
export function sliceAnchoredLines(
  content: string,
  startLine: number,
  endLine?: number | null,
): string[] {
  const lines = splitContentLines(content);
  const start = Math.max(1, startLine);
  const end = endLine != null && endLine >= start ? endLine : start;
  return lines.slice(start - 1, end);
}

/// ONE synthetic hunk: remove the original lines, then add the replacement
/// lines. Both sides are numbered from `startLine`. Empty replacement is a
/// pure deletion; empty original is a pure insertion.
export function synthesizeSuggestionDiff(
  originalLines: string[],
  replacement: string,
  startLine: number,
): ParsedDiff {
  const addLines = splitSuggestionLines(replacement);
  const delCount = originalLines.length;
  const addCount = addLines.length;
  const start = startLine > 0 ? startLine : 1;
  const header = `@@ -${start},${delCount} +${start},${addCount} @@`;

  const lines: DiffLine[] = [];
  let oldLine = start;
  for (const text of originalLines) {
    lines.push({ kind: "remove", text, oldLine, newLine: null });
    oldLine += 1;
  }
  let newLine = start;
  for (const text of addLines) {
    lines.push({ kind: "add", text, oldLine: null, newLine });
    newLine += 1;
  }

  return {
    preamble: [],
    hunks: [
      {
        header,
        oldStart: start,
        oldLines: delCount,
        newStart: start,
        newLines: addCount,
        lines,
      },
    ],
    binary: false,
  };
}

/// True when the anchor no longer resolves (`resolution.orphaned`).
/// That is the ONLY client-derivable outdated signal. A 409 at apply
/// time is the other (server-side) signal: the working-tree range no
/// longer byte-equals `suggestion.original`.
export function suggestionIsOutdated(
  comment: Pick<ReviewComment, "resolution">,
): boolean {
  return comment.resolution.orphaned;
}

/// New-side, live, `line|range` threads can open the suggestion editor.
/// Old-side / orphaned / other kinds render nothing.
export function threadAcceptsSuggestion(
  comment: Pick<ReviewComment, "side" | "anchor_kind" | "resolution">,
): boolean {
  if (comment.side === "old") return false;
  if (comment.resolution.orphaned) return false;
  return comment.anchor_kind === "line" || comment.anchor_kind === "range";
}

const SNIPPET_MAX = 36;

function compactSnippet(text: string): string {
  const one = text.replace(/\s+/g, " ").trim();
  if (one === "") return '""';
  const cut = one.length > SNIPPET_MAX ? `${one.slice(0, SNIPPET_MAX)}…` : one;
  return `"${cut}"`;
}

/// Compact expected-vs-found line for a 409 apply toast — readable, not
/// a wall of the two full ranges.
export function formatApplyConflictHint(input: {
  expected: string;
  found: string;
  resolved_line?: number;
}): string {
  const at =
    input.resolved_line != null && input.resolved_line > 0
      ? ` at L${input.resolved_line}`
      : "";
  return `Can't apply${at}: expected ${compactSnippet(input.expected)}, found ${compactSnippet(input.found)}`;
}
