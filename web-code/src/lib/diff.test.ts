import { describe, expect, it } from "vitest";
import { changedNewLineRanges, diffStats, firstChangedLine, parseUnifiedDiff } from "./diff";

const TWO_HUNK_DIFF = `diff --git a/a.txt b/a.txt
index e69de29..0cfbf08 100644
--- a/a.txt
+++ b/a.txt
@@ -1,3 +1,3 @@
 line1
-line2
+CHANGED
 line3
@@ -10,2 +10,3 @@
 line10
+line11
`;

describe("parseUnifiedDiff", () => {
  it("returns an empty parse for empty diff text (no textual difference)", () => {
    const parsed = parseUnifiedDiff("");
    expect(parsed).toEqual({ preamble: [], hunks: [], binary: false });
  });

  it("returns an empty parse for whitespace-only diff text", () => {
    const parsed = parseUnifiedDiff("\n");
    expect(parsed.hunks).toEqual([]);
    expect(parsed.binary).toBe(false);
  });

  it("splits preamble from hunks", () => {
    const parsed = parseUnifiedDiff(TWO_HUNK_DIFF);
    expect(parsed.preamble).toEqual([
      "diff --git a/a.txt b/a.txt",
      "index e69de29..0cfbf08 100644",
      "--- a/a.txt",
      "+++ b/a.txt",
    ]);
    expect(parsed.hunks).toHaveLength(2);
  });

  it("parses hunk header ranges", () => {
    const parsed = parseUnifiedDiff(TWO_HUNK_DIFF);
    expect(parsed.hunks[0]).toMatchObject({
      oldStart: 1,
      oldLines: 3,
      newStart: 1,
      newLines: 3,
    });
    expect(parsed.hunks[1]).toMatchObject({
      oldStart: 10,
      oldLines: 2,
      newStart: 10,
      newLines: 3,
    });
  });

  it("classifies context/add/remove lines and strips the leading marker", () => {
    const parsed = parseUnifiedDiff(TWO_HUNK_DIFF);
    const lines = parsed.hunks[0].lines;
    expect(lines).toEqual([
      { kind: "context", text: "line1", oldLine: 1, newLine: 1 },
      { kind: "remove", text: "line2", oldLine: 2, newLine: null },
      { kind: "add", text: "CHANGED", oldLine: null, newLine: 2 },
      { kind: "context", text: "line3", oldLine: 3, newLine: 3 },
    ]);
  });

  it("tracks old/new line numbers correctly across an add-only hunk", () => {
    const parsed = parseUnifiedDiff(TWO_HUNK_DIFF);
    const lines = parsed.hunks[1].lines;
    expect(lines).toEqual([
      { kind: "context", text: "line10", oldLine: 10, newLine: 10 },
      { kind: "add", text: "line11", oldLine: null, newLine: 11 },
    ]);
  });

  it("detects a binary-file diff", () => {
    const text = "diff --git a/bin.dat b/bin.dat\nBinary files a/bin.dat and b/bin.dat differ\n";
    const parsed = parseUnifiedDiff(text);
    expect(parsed.binary).toBe(true);
    expect(parsed.hunks).toEqual([]);
  });

  it("drops a 'no newline at end of file' marker line", () => {
    const text = "@@ -1,1 +1,1 @@\n-old\n\\ No newline at end of file\n+new\n";
    const parsed = parseUnifiedDiff(text);
    expect(parsed.hunks[0].lines.map((l) => l.text)).toEqual(["old", "new"]);
  });

  it("treats a hunk header with a single implicit line count as count 1", () => {
    const text = "@@ -5 +5 @@\n context\n";
    const parsed = parseUnifiedDiff(text);
    expect(parsed.hunks[0]).toMatchObject({ oldLines: 1, newLines: 1, oldStart: 5, newStart: 5 });
  });
});

describe("diffStats", () => {
  it("counts additions and deletions across every hunk", () => {
    const parsed = parseUnifiedDiff(TWO_HUNK_DIFF);
    expect(diffStats(parsed)).toEqual({ additions: 2, deletions: 1 });
  });

  it("is zero for a diff with no hunks", () => {
    expect(diffStats(parseUnifiedDiff(""))).toEqual({ additions: 0, deletions: 0 });
  });
});

describe("changedNewLineRanges", () => {
  it("collapses a single added line into a one-line range", () => {
    const parsed = parseUnifiedDiff(TWO_HUNK_DIFF);
    // Hunk 0: line2 removed, CHANGED added at new-line 2.
    expect(changedNewLineRanges({ preamble: [], hunks: [parsed.hunks[0]], binary: false })).toEqual([
      { start: 2, end: 2 },
    ]);
  });

  it("merges a contiguous run of added lines into one range", () => {
    const text = "@@ -1,1 +1,4 @@\n context\n+a\n+b\n+c\n";
    const parsed = parseUnifiedDiff(text);
    expect(changedNewLineRanges(parsed)).toEqual([{ start: 2, end: 4 }]);
  });

  it("splits two add runs separated by context into two ranges", () => {
    const text = "@@ -1,3 +1,5 @@\n before\n+add1\n context\n+add2\n after\n";
    const parsed = parseUnifiedDiff(text);
    expect(changedNewLineRanges(parsed)).toEqual([
      { start: 2, end: 2 },
      { start: 4, end: 4 },
    ]);
  });

  it("returns one range spanning the whole file for a from-scratch add (the oldest commit case)", () => {
    const text = "@@ -0,0 +1,3 @@\n+one\n+two\n+three\n";
    const parsed = parseUnifiedDiff(text);
    expect(changedNewLineRanges(parsed)).toEqual([{ start: 1, end: 3 }]);
  });

  it("returns no ranges for a pure-deletion diff", () => {
    const text = "@@ -1,2 +1,0 @@\n-gone1\n-gone2\n";
    const parsed = parseUnifiedDiff(text);
    expect(changedNewLineRanges(parsed)).toEqual([]);
  });

  it("returns no ranges for an empty diff", () => {
    expect(changedNewLineRanges(parseUnifiedDiff(""))).toEqual([]);
  });
});

describe("firstChangedLine", () => {
  it("returns the first range's start", () => {
    expect(firstChangedLine([{ start: 4, end: 6 }, { start: 10, end: 10 }])).toBe(4);
  });

  it("returns undefined for no ranges", () => {
    expect(firstChangedLine([])).toBeUndefined();
  });
});
