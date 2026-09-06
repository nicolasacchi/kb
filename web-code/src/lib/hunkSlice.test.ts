import { describe, expect, it } from "vitest";
import { parseUnifiedDiff } from "./diff";
import { HOVER_DIFF_MAX_LINES, sliceHunksForLine, slicedAsParsed } from "./hunkSlice";

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

describe("sliceHunksForLine", () => {
  it("returns empty for empty/binary diffs", () => {
    expect(sliceHunksForLine(parseUnifiedDiff(""), { newLine: 1 }).hunks).toEqual([]);
    const bin = parseUnifiedDiff("Binary files a/x and b/x differ\n");
    expect(sliceHunksForLine(bin, { newLine: 1 }).hunks).toEqual([]);
  });

  it("returns empty when no line target is given", () => {
    const parsed = parseUnifiedDiff(TWO_HUNK_DIFF);
    expect(sliceHunksForLine(parsed, {}).hunks).toEqual([]);
  });

  it("keeps only the hunk covering a NEW-side line", () => {
    const parsed = parseUnifiedDiff(TWO_HUNK_DIFF);
    // Line 2 NEW is the CHANGED add in the first hunk.
    const slice = sliceHunksForLine(parsed, { newLine: 2 });
    expect(slice.hunks).toHaveLength(1);
    expect(slice.hunks[0].header).toContain("@@ -1,3 +1,3 @@");
    expect(slice.truncated).toBe(false);
  });

  it("keeps the second hunk for a later NEW-side line", () => {
    const parsed = parseUnifiedDiff(TWO_HUNK_DIFF);
    const slice = sliceHunksForLine(parsed, { newLine: 11 });
    expect(slice.hunks).toHaveLength(1);
    expect(slice.hunks[0].header).toContain("@@ -10,2 +10,3 @@");
  });

  it("returns empty when the target line is outside every hunk", () => {
    const parsed = parseUnifiedDiff(TWO_HUNK_DIFF);
    const slice = sliceHunksForLine(parsed, { newLine: 50 });
    expect(slice.hunks).toEqual([]);
    expect(slice.truncated).toBe(false);
  });

  it("matches on OLD-side line numbers too", () => {
    const parsed = parseUnifiedDiff(TWO_HUNK_DIFF);
    // old line 2 is the removed "line2".
    const slice = sliceHunksForLine(parsed, { oldLine: 2 });
    expect(slice.hunks).toHaveLength(1);
    expect(slice.hunks[0].lines.some((l) => l.kind === "remove" && l.text === "line2")).toBe(true);
  });

  it("caps rendered lines at the default budget and sets truncated", () => {
    // Build a single large hunk with many context lines so the cap fires.
    const lines = ["diff --git a/b.txt b/b.txt", "--- a/b.txt", "+++ b/b.txt", "@@ -1,60 +1,60 @@"];
    for (let i = 1; i <= 60; i++) {
      lines.push(` line${i}`);
    }
    const parsed = parseUnifiedDiff(lines.join("\n") + "\n");
    const slice = sliceHunksForLine(parsed, { newLine: 5 });
    expect(slice.truncated).toBe(true);
    expect(slice.keptLines).toBe(HOVER_DIFF_MAX_LINES);
    // 1 header + (max-1) content lines.
    expect(slice.hunks[0].lines.length).toBe(HOVER_DIFF_MAX_LINES - 1);
  });

  it("honours a custom maxLines", () => {
    const parsed = parseUnifiedDiff(TWO_HUNK_DIFF);
    const slice = sliceHunksForLine(parsed, { newLine: 2, maxLines: 3 });
    // header + 2 content lines of the first hunk (4 total available).
    expect(slice.keptLines).toBe(3);
    expect(slice.truncated).toBe(true);
    expect(slice.hunks[0].lines).toHaveLength(2);
  });

  it("slicedAsParsed rebuilds a DiffHunks-ready ParsedDiff", () => {
    const parsed = parseUnifiedDiff(TWO_HUNK_DIFF);
    const slice = sliceHunksForLine(parsed, { newLine: 2 });
    const rebuilt = slicedAsParsed(slice);
    expect(rebuilt.preamble).toEqual([]);
    expect(rebuilt.binary).toBe(false);
    expect(rebuilt.hunks).toEqual(slice.hunks);
  });
});
