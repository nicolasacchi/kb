// Client-side unified-diff parsing (`DiffResponse.diff`,
// `crates/kb-code-server/src/diff.rs`'s raw `git diff --no-color -U3`
// output). The server deliberately returns TEXT, not a pre-structured hunk
// array (see that module's doc) — this is where the structuring happens,
// once, colocated with `DiffView`'s render logic and independently
// vitest-covered rather than only exercised end-to-end.
//
// Rendering choice: `DiffView` renders UNIFIED (not side-by-side) diffs —
// a CM6 `@codemirror/merge` side-by-side view was considered but deferred
// (not in this crate's dependency set, and a unified render needs no extra
// CM6 machinery beyond what `CodeView` already has for line numbers). See
// `DiffView.tsx`'s own header comment.

export type DiffLineKind = "context" | "add" | "remove" | "header" | "hunk";

export interface DiffLine {
  kind: DiffLineKind;
  text: string;
  /// 1-based line number in the "from" file, or `null` for an add/header/
  /// hunk line (git's own unified-diff convention: an added line has no
  /// position in the pre-image).
  oldLine: number | null;
  /// 1-based line number in the "to" file, or `null` for a remove/header/
  /// hunk line.
  newLine: number | null;
}

export interface DiffHunk {
  header: string;
  oldStart: number;
  oldLines: number;
  newStart: number;
  newLines: number;
  lines: DiffLine[];
}

export interface ParsedDiff {
  /// `diff --git`/`index`/`---`/`+++` preamble lines, verbatim — rendered
  /// (if at all) as a collapsed header, never parsed further.
  preamble: string[];
  hunks: DiffHunk[];
  /// `true` when the diff is git's own `Binary files a/... and b/... differ`
  /// one-liner (or the diff text is otherwise hunk-less but non-empty) —
  /// `DiffView` shows a "binary file" placeholder instead of line rows.
  binary: boolean;
}

const HUNK_HEADER = /^@@ -(\d+)(?:,(\d+))? \+(\d+)(?:,(\d+))? @@.*$/;

/// Parse `git diff`'s unified-diff text into a preamble + hunk list. Never
/// throws — a malformed/truncated hunk header is skipped (its lines fold
/// into the running hunk if one is open, else are dropped), matching this
/// module's role as a best-effort RENDERING aid, not a diff-correctness
/// oracle (the server-side `git diff` subprocess already owns correctness).
export function parseUnifiedDiff(text: string): ParsedDiff {
  if (text.trim() === "") {
    return { preamble: [], hunks: [], binary: false };
  }
  const lines = text.split("\n");
  // Drop a single trailing empty element from the final "\n" split (a
  // real diff always ends with a newline) so no phantom empty context
  // line appears in the last hunk.
  if (lines.length > 0 && lines[lines.length - 1] === "") lines.pop();

  if (lines.some((l) => l.startsWith("Binary files "))) {
    return { preamble: lines, hunks: [], binary: true };
  }

  const preamble: string[] = [];
  const hunks: DiffHunk[] = [];
  let current: DiffHunk | null = null;
  let oldLine = 0;
  let newLine = 0;

  for (const line of lines) {
    const headerMatch = line.match(HUNK_HEADER);
    if (headerMatch) {
      oldLine = parseInt(headerMatch[1], 10);
      newLine = parseInt(headerMatch[3], 10);
      current = {
        header: line,
        oldStart: oldLine,
        oldLines: headerMatch[2] !== undefined ? parseInt(headerMatch[2], 10) : 1,
        newStart: newLine,
        newLines: headerMatch[4] !== undefined ? parseInt(headerMatch[4], 10) : 1,
        lines: [],
      };
      hunks.push(current);
      continue;
    }
    if (current === null) {
      // Before the first hunk — preamble (diff --git / index / ---/+++).
      preamble.push(line);
      continue;
    }
    if (line.startsWith("+")) {
      current.lines.push({ kind: "add", text: line.slice(1), oldLine: null, newLine });
      newLine += 1;
    } else if (line.startsWith("-")) {
      current.lines.push({ kind: "remove", text: line.slice(1), oldLine, newLine: null });
      oldLine += 1;
    } else if (line.startsWith("\\")) {
      // "\ No newline at end of file" — not a real content line.
      continue;
    } else {
      // A context line's leading space is git's own alignment column, not
      // file content.
      current.lines.push({
        kind: "context",
        text: line.startsWith(" ") ? line.slice(1) : line,
        oldLine,
        newLine,
      });
      oldLine += 1;
      newLine += 1;
    }
  }

  return { preamble, hunks, binary: false };
}

/// Total `+`/`-` counts across every hunk — the reader's diff-view header
/// stat line.
export function diffStats(parsed: ParsedDiff): { additions: number; deletions: number } {
  let additions = 0;
  let deletions = 0;
  for (const hunk of parsed.hunks) {
    for (const line of hunk.lines) {
      if (line.kind === "add") additions += 1;
      else if (line.kind === "remove") deletions += 1;
    }
  }
  return { additions, deletions };
}

/// A contiguous run of `add` lines' NEW-side line numbers (1-based,
/// inclusive both ends) — Phase C7's story player tints these per step
/// (`editor/storyOverlay.ts`) and auto-scrolls to the first one.
export interface ChangedRange {
  start: number;
  end: number;
}

/// Every contiguous run of added lines across every hunk, in hunk/document
/// order — a context or removed line always breaks a run (so two adds
/// separated by unchanged context become two ranges, never merged into
/// one). For a file's OLDEST commit (diffed against its non-existent
/// pre-image, i.e. `from=<sha>^` on a commit that ADDS the file) every
/// content line is an `add`, so this naturally returns one range spanning
/// the whole file — no special-casing needed at this layer.
export function changedNewLineRanges(parsed: ParsedDiff): ChangedRange[] {
  const ranges: ChangedRange[] = [];
  for (const hunk of parsed.hunks) {
    let start: number | null = null;
    let end: number | null = null;
    for (const line of hunk.lines) {
      if (line.kind === "add" && line.newLine !== null) {
        if (start === null) start = line.newLine;
        end = line.newLine;
      } else if (start !== null) {
        ranges.push({ start, end: end ?? start });
        start = null;
        end = null;
      }
    }
    if (start !== null) ranges.push({ start, end: end ?? start });
  }
  return ranges;
}

/// The first changed range's start line — the story player's auto-scroll
/// target for a step. `undefined` for a diff with no additions at all (a
/// pure-deletion step, or a diff that hasn't loaded/failed to fetch) —
/// callers treat that as "don't scroll," never a crash.
export function firstChangedLine(ranges: ChangedRange[]): number | undefined {
  return ranges[0]?.start;
}
