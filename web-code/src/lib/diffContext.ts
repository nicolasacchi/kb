// V73-K2a — the context dial and "expand N more lines" (design §D9:
// "expand/collapse all with a context dial (hunk → whole file → file with
// reader gutters, in place)").
//
// THE ONE RULE: expanded context is REAL FILE CONTENT, fetched from the
// server, never synthesised. `GET /api/diff` hardcodes `git diff -U3`
// (`diff.rs:97-132` — there is no context parameter on that route and this
// unit does not add one), so widening the window means going to the file
// itself: `GET /api/file?repo=&path=&ref=<patchset tip sha>` returns the
// whole blob, and this module SPLICES the requested rows out of it. If
// that fetch has not landed (or the file is binary, or the ref is gone),
// the hunk renders at its original width with a caption — never a filled-
// in gap, never a "…" that pretends to be code.
//
// The line arithmetic is the part worth getting exactly right, so it lives
// here, pure and unit-pinned, instead of inside a renderer. A unified
// hunk's header gives four numbers (`-oldStart,oldLines +newStart,newLines`).
// A context row ABOVE the hunk therefore sits at old = new − (newStart −
// oldStart); a row BELOW sits at old = new − (newEnd − oldEnd), where the
// two ends are `start + lines`. Those two deltas differ by exactly the
// hunk's own net add/remove, which is why using one for both — the obvious
// simplification — silently mislabels every gutter number underneath a
// hunk that is not size-neutral.

import type { DiffHunk, DiffLine, ParsedDiff } from "./diff";
import type { DiffCtxDial } from "./codeUrl";

/// How many context lines each dial stop shows on each side. `3` is git's
/// own `-U3` (what the wire already returns — no fetch needed);
/// `Number.POSITIVE_INFINITY` for `"full"`, which clamps to the file's own
/// bounds below.
export function ctxLinesFor(dial: DiffCtxDial): number {
  if (dial === "full") return Number.POSITIVE_INFINITY;
  return dial === 10 ? 10 : 3;
}

/// One click of "expand more" adds this many rows on that side. Named so
/// the button label and the arithmetic cannot drift.
export const EXPAND_STEP = 10;

/// Split a file's text into lines the same way a 1-based gutter numbers
/// them: line N is `lines[N - 1]`. A trailing newline does NOT create a
/// phantom final line.
export function fileLines(content: string): string[] {
  const lines = content.split("\n");
  if (lines.length > 0 && lines[lines.length - 1] === "") lines.pop();
  return lines;
}

/// The NEW-side window a hunk already covers, INCLUDING the `-U3` context
/// rows the wire sent. Derived from the parsed rows rather than from the
/// header, because git omits `,count` for a 1-line side and a header-only
/// derivation gets that case wrong.
function renderedNewWindow(hunk: DiffHunk): { first: number; last: number } | null {
  let first: number | null = null;
  let last: number | null = null;
  for (const line of hunk.lines) {
    if (line.newLine === null) continue;
    if (first === null) first = line.newLine;
    last = line.newLine;
  }
  return first === null || last === null ? null : { first, last };
}

/// The old-side offset for a context row ABOVE the hunk and one BELOW it.
/// See this module's header for why they are two numbers, not one.
function deltas(hunk: DiffHunk): { above: number; below: number } {
  const above = hunk.newStart - hunk.oldStart;
  const below = hunk.newStart + hunk.newLines - (hunk.oldStart + hunk.oldLines);
  return { above, below };
}

export interface ExpandRequest {
  /// Extra rows wanted ABOVE what the wire already sent.
  before: number;
  /// Extra rows wanted BELOW.
  after: number;
}

/// The extra rows a dial stop asks for, over and above git's own `-U3`.
/// `full` asks for everything; the splice clamps it to the file.
export function expandForDial(dial: DiffCtxDial): ExpandRequest {
  const want = ctxLinesFor(dial);
  if (want === Number.POSITIVE_INFINITY) {
    return { before: Number.POSITIVE_INFINITY, after: Number.POSITIVE_INFINITY };
  }
  const extra = Math.max(0, want - 3);
  return { before: extra, after: extra };
}

export interface ExpandedHunk {
  /// The rows to render, in order: spliced context, then the wire's own
  /// rows, then spliced context.
  lines: DiffLine[];
  /// How many rows were actually added on each side after clamping — what
  /// the "expand 10 more" button reports and what makes it go away at the
  /// top/bottom of the file.
  addedBefore: number;
  addedAfter: number;
  /// True when there is still file above / below this window to reveal.
  moreAbove: boolean;
  moreBelow: boolean;
}

/// Splice `req` extra context rows around `hunk`, taken verbatim from
/// `lines` (the NEW-side file at the patchset tip). Returns the hunk
/// unchanged — with `addedBefore/addedAfter: 0` and honest `moreAbove/
/// moreBelow` flags — when the file content is not available, which is the
/// "never fabricate" branch this module exists to make explicit.
export function expandHunk(
  hunk: DiffHunk,
  lines: readonly string[] | null,
  req: ExpandRequest,
): ExpandedHunk {
  const win = renderedNewWindow(hunk);
  if (!win) {
    // A pure-deletion hunk has no new-side anchor to splice around. The
    // old side would need the file at the BASE sha, a second fetch this
    // unit deliberately does not add — so it renders at its wire width
    // and says nothing further is available.
    return { lines: hunk.lines, addedBefore: 0, addedAfter: 0, moreAbove: false, moreBelow: false };
  }
  if (!lines) {
    return {
      lines: hunk.lines,
      addedBefore: 0,
      addedAfter: 0,
      moreAbove: win.first > 1,
      moreBelow: true,
    };
  }
  const { above, below } = deltas(hunk);
  const total = lines.length;

  const wantBefore = req.before === Number.POSITIVE_INFINITY ? win.first - 1 : req.before;
  const startAt = Math.max(1, win.first - Math.max(0, wantBefore));
  const before: DiffLine[] = [];
  for (let n = startAt; n < win.first; n++) {
    before.push({ kind: "context", text: lines[n - 1] ?? "", oldLine: n - above, newLine: n });
  }

  const wantAfter = req.after === Number.POSITIVE_INFINITY ? total - win.last : req.after;
  const endAt = Math.min(total, win.last + Math.max(0, wantAfter));
  const after: DiffLine[] = [];
  for (let n = win.last + 1; n <= endAt; n++) {
    after.push({ kind: "context", text: lines[n - 1] ?? "", oldLine: n - below, newLine: n });
  }

  return {
    lines: [...before, ...hunk.lines, ...after],
    addedBefore: before.length,
    addedAfter: after.length,
    moreAbove: startAt > 1,
    moreBelow: endAt < total,
  };
}

/// The full expand request for one hunk: the dial's own width plus
/// whatever the operator has clicked "expand more" for on this hunk.
/// Additive by construction, so the dial and the buttons never fight over
/// one number.
export function combineExpand(dial: DiffCtxDial, manual: ExpandRequest | undefined): ExpandRequest {
  const base = expandForDial(dial);
  if (!manual) return base;
  return {
    before: base.before === Number.POSITIVE_INFINITY ? base.before : base.before + manual.before,
    after: base.after === Number.POSITIVE_INFINITY ? base.after : base.after + manual.after,
  };
}

/// Does this dial stop need the file's content at all? `3` does not (the
/// wire already sent it), which is what keeps the default page byte-for-
/// byte as cheap as it was before diff v2 — no extra request per file
/// unless the operator turns the dial.
export function dialNeedsFile(dial: DiffCtxDial): boolean {
  return dial !== 3;
}

/// The caption a hunk shows when a wider dial is selected but the file
/// content has not arrived (or cannot). `null` when there is nothing to
/// say. Never a spinner standing in for content: the rows on screen are
/// always the rows the server sent.
export function contextCaption(
  dial: DiffCtxDial,
  fileAvailable: boolean,
  loading: boolean,
): string | null {
  if (!dialNeedsFile(dial) || fileAvailable) return null;
  return loading
    ? "widening context — fetching the file at this patchset"
    : "context not widened: this file's content is unavailable at this patchset";
}

/// Every hunk of a parsed file, expanded — the renderer's one call.
export function expandParsed(
  parsed: ParsedDiff,
  lines: readonly string[] | null,
  dial: DiffCtxDial,
  manualByHunk: ReadonlyMap<number, ExpandRequest> | null,
): ExpandedHunk[] {
  return parsed.hunks.map((h, i) =>
    expandHunk(h, lines, combineExpand(dial, manualByHunk?.get(i))),
  );
}
