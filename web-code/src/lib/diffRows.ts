// Pure side-by-side row builder over `parseUnifiedDiff`'s hunk list.
// One parsed model, two renderers (V4.D1): UnifiedHunks walks
// `ParsedDiff.hunks` as-is; SplitHunks walks `buildSplitRows` so pairing
// lives in one place, independently vitest-covered.

import type { DiffLine, ParsedDiff } from "./diff";

export type SplitRow =
  | { kind: "hunk"; header: string }
  | { kind: "pair"; old: DiffLine | null; new: DiffLine | null };

/// Per hunk: emit a header row, then walk lines as context runs and
/// change runs (maximal runs of non-context lines). A context line is
/// `pair(line, line)`. A change run's removes `R` and adds `A` pair
/// index-wise, with spacer tails for the longer side. Git orders
/// removes-before-adds; an interleaved run is still partitioned by kind
/// (order preserved within each side) so the output stays correct even
/// when unpaired.
export function buildSplitRows(parsed: ParsedDiff): SplitRow[] {
  const rows: SplitRow[] = [];
  for (const hunk of parsed.hunks) {
    rows.push({ kind: "hunk", header: hunk.header });
    rows.push(...buildSplitPairs(hunk.lines));
  }
  return rows;
}

/// V73-K2a — the pairing half of `buildSplitRows`, over an ARBITRARY line
/// list rather than a whole `ParsedDiff`. Diff v2's split renderer walks
/// one hunk at a time (each hunk owns a header STRIP, a fold and its own
/// context-expanded rows, so a flat "header row then pairs" stream can no
/// longer be sliced back apart), and the context dial hands it lines that
/// are not `hunk.lines` any more. Extracted rather than duplicated:
/// `buildSplitRows` above now delegates to it, so the two can never
/// disagree about pairing, and `diffRows.test.ts`'s existing golden still
/// walks the original entry point unchanged.
export function buildSplitPairs(lines: readonly DiffLine[]): SplitRow[] {
  const rows: SplitRow[] = [];
  {
    let i = 0;
    while (i < lines.length) {
      const line = lines[i];
      if (line.kind === "context") {
        rows.push({ kind: "pair", old: line, new: line });
        i += 1;
        continue;
      }
      const removes: DiffLine[] = [];
      const adds: DiffLine[] = [];
      while (i < lines.length && lines[i].kind !== "context") {
        const runLine = lines[i];
        if (runLine.kind === "remove") removes.push(runLine);
        else if (runLine.kind === "add") adds.push(runLine);
        i += 1;
      }
      const paired = Math.min(removes.length, adds.length);
      for (let k = 0; k < paired; k++) {
        rows.push({ kind: "pair", old: removes[k], new: adds[k] });
      }
      for (let k = paired; k < removes.length; k++) {
        rows.push({ kind: "pair", old: removes[k], new: null });
      }
      for (let k = paired; k < adds.length; k++) {
        rows.push({ kind: "pair", old: null, new: adds[k] });
      }
    }
  }
  return rows;
}
