import { describe, expect, it } from "vitest";
import { parseUnifiedDiff } from "./diff";
import { buildSplitRows } from "./diffRows";
import type { SplitRow } from "./diffRows";

function pairTexts(rows: SplitRow[]): Array<{ old: string | null; new: string | null }> {
  return rows
    .filter((r): r is Extract<SplitRow, { kind: "pair" }> => r.kind === "pair")
    .map((r) => ({ old: r.old?.text ?? null, new: r.new?.text ?? null }));
}

function hunkHeaders(rows: SplitRow[]): string[] {
  return rows.filter((r): r is Extract<SplitRow, { kind: "hunk" }> => r.kind === "hunk").map((r) => r.header);
}

describe("buildSplitRows", () => {
  it("returns no rows for an empty diff", () => {
    expect(buildSplitRows(parseUnifiedDiff(""))).toEqual([]);
  });

  it("emits a hunk header then pair(line, line) for a pure-context hunk", () => {
    const parsed = parseUnifiedDiff("@@ -1,2 +1,2 @@\n one\n two\n");
    const rows = buildSplitRows(parsed);
    expect(hunkHeaders(rows)).toEqual(["@@ -1,2 +1,2 @@"]);
    expect(pairTexts(rows)).toEqual([
      { old: "one", new: "one" },
      { old: "two", new: "two" },
    ]);
  });

  it("pairs a pure-add hunk against null old cells", () => {
    const parsed = parseUnifiedDiff("@@ -0,0 +1,2 @@\n+a\n+b\n");
    expect(pairTexts(buildSplitRows(parsed))).toEqual([
      { old: null, new: "a" },
      { old: null, new: "b" },
    ]);
  });

  it("pairs a pure-remove hunk against null new cells", () => {
    const parsed = parseUnifiedDiff("@@ -1,2 +0,0 @@\n-a\n-b\n");
    expect(pairTexts(buildSplitRows(parsed))).toEqual([
      { old: "a", new: null },
      { old: "b", new: null },
    ]);
  });

  it("pairs a balanced replace index-wise", () => {
    const parsed = parseUnifiedDiff("@@ -1,3 +1,3 @@\n keep\n-old\n+new\n keep2\n");
    expect(pairTexts(buildSplitRows(parsed))).toEqual([
      { old: "keep", new: "keep" },
      { old: "old", new: "new" },
      { old: "keep2", new: "keep2" },
    ]);
  });

  it("unbalanced replace (more removes) leaves old-side spacer tails", () => {
    const parsed = parseUnifiedDiff("@@ -1,3 +1,2 @@\n ctx\n-a\n-b\n+c\n");
    expect(pairTexts(buildSplitRows(parsed))).toEqual([
      { old: "ctx", new: "ctx" },
      { old: "a", new: "c" },
      { old: "b", new: null },
    ]);
  });

  it("unbalanced replace (more adds) leaves new-side spacer tails", () => {
    const parsed = parseUnifiedDiff("@@ -1,2 +1,3 @@\n ctx\n-a\n+b\n+c\n");
    expect(pairTexts(buildSplitRows(parsed))).toEqual([
      { old: "ctx", new: "ctx" },
      { old: "a", new: "b" },
      { old: null, new: "c" },
    ]);
  });

  it("emits one header per hunk and does not merge across hunks", () => {
    const text = `@@ -1,2 +1,2 @@
 keep
-old
+new
@@ -10,1 +10,2 @@
 ten
+eleven
`;
    const rows = buildSplitRows(parseUnifiedDiff(text));
    expect(hunkHeaders(rows)).toEqual(["@@ -1,2 +1,2 @@", "@@ -10,1 +10,2 @@"]);
    expect(pairTexts(rows)).toEqual([
      { old: "keep", new: "keep" },
      { old: "old", new: "new" },
      { old: "ten", new: "ten" },
      { old: null, new: "eleven" },
    ]);
  });

  it("partitions an interleaved change run by kind, preserving order", () => {
    // Pathological: adds before removes, then another add. One change run.
    const parsed = parseUnifiedDiff("@@ -1,3 +1,3 @@\n+b\n-a\n+d\n-c\n");
    expect(pairTexts(buildSplitRows(parsed))).toEqual([
      { old: "a", new: "b" },
      { old: "c", new: "d" },
    ]);
  });
});
