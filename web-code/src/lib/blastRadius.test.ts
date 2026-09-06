import { describe, expect, it } from "vitest";
import type { Symbol } from "../api/types";
import { parseUnifiedDiff } from "./diff";
import { changedNewLines, symbolsInChangedHunks } from "./blastRadius";

const DIFF = `diff --git a/lib.rs b/lib.rs
--- a/lib.rs
+++ b/lib.rs
@@ -1,5 +1,7 @@
 fn keep() {}
+fn added_one() {}
 fn mid() {}
+fn added_two() {}
 fn end() {}
`;

function sym(name: string, start: number, end = start): Symbol {
  return {
    ordinal: start,
    name,
    kind: "function",
    line_start: start,
    line_end: end,
    col_start: 0,
    col_end: 0,
    container: null,
    signature: null,
    doc: null,
  };
}

describe("blastRadius", () => {
  it("collects added new-lines from a unified diff", () => {
    const parsed = parseUnifiedDiff(DIFF);
    const lines = changedNewLines(parsed);
    // After parse: keep=1, added_one=2, mid=3, added_two=4, end=5
    expect([...lines].sort((a, b) => a - b)).toEqual([2, 4]);
  });

  it("picks top ≤3 symbols covering changed lines", () => {
    const symbols = [
      sym("keep", 1),
      sym("added_one", 2),
      sym("mid", 3),
      sym("added_two", 4),
      sym("end", 5),
    ];
    const hits = symbolsInChangedHunks(symbols, new Set([2, 4]), 3);
    expect(hits.map((s) => s.name)).toEqual(["added_one", "added_two"]);
  });

  it("returns empty when no overlap", () => {
    expect(symbolsInChangedHunks([sym("keep", 1)], new Set([99]))).toEqual([]);
  });
});
