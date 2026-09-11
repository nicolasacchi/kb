// V76-R2b — the added-file empty-center regression.
//
// A new file's hunk is `@@ -0,0 +1,N @@` with N add lines and no old
// side. Split view pairs those as `{old: null, new: add}` — the old cell
// is the hatched spacer. The body must still carry every add line after
// parse, after the context dial, and after split pairing. A prior render
// showed the hunk strip (`+N −0`) over an empty hatch because the new
// column clipped to zero (`1fr` min-content vs a long hunk header).

import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";
import { describe, expect, it } from "vitest";
import { parseUnifiedDiff } from "./diff";
import { expandForDial, expandHunk, fileLines } from "./diffContext";
import { buildSplitPairs } from "./diffRows";
import { hunkStats } from "./diffHunks";

function addedDiff(n: number): string {
  const body = Array.from({ length: n }, (_, i) => `+line ${i + 1}`).join("\n");
  return `diff --git a/app/new.rb b/app/new.rb
new file mode 100644
index 0000000..abc123
--- /dev/null
+++ b/app/new.rb
@@ -0,0 +1,${n} @@
${body}
`;
}

describe("added-file hunk render path (@@ -0,0 +1,N @@)", () => {
  it("parses N add lines with no old-side numbers", () => {
    const parsed = parseUnifiedDiff(addedDiff(48));
    expect(parsed.hunks).toHaveLength(1);
    expect(parsed.hunks[0].header).toContain("@@ -0,0 +1,48 @@");
    expect(parsed.hunks[0].oldStart).toBe(0);
    expect(parsed.hunks[0].oldLines).toBe(0);
    expect(parsed.hunks[0].newStart).toBe(1);
    expect(parsed.hunks[0].newLines).toBe(48);
    expect(parsed.hunks[0].lines).toHaveLength(48);
    expect(parsed.hunks[0].lines.every((l) => l.kind === "add" && l.oldLine === null && l.newLine !== null)).toBe(
      true,
    );
    expect(hunkStats(parsed.hunks[0])).toEqual({ additions: 48, deletions: 0 });
  });

  it("keeps every add line after ctx=full splice (the file IS the hunk)", () => {
    const hunk = parseUnifiedDiff(addedDiff(48)).hunks[0];
    const lines = fileLines(Array.from({ length: 48 }, (_, i) => `line ${i + 1}`).join("\n") + "\n");
    const out = expandHunk(hunk, lines, expandForDial("full"));
    expect(out.lines.filter((l) => l.kind === "add")).toHaveLength(48);
    expect(out.lines[0].kind).toBe("add");
    expect(out.lines[0].text).toBe("line 1");
    expect(out.lines[47].text).toBe("line 48");
  });

  it("keeps the wire lines when the file fetch is empty or missing — never a blank body", () => {
    const hunk = parseUnifiedDiff(addedDiff(8)).hunks[0];
    expect(expandHunk(hunk, null, expandForDial("full")).lines).toEqual(hunk.lines);
    expect(expandHunk(hunk, [], expandForDial(10)).lines).toEqual(hunk.lines);
  });

  it("split-pairs every add against a null old cell (the hatch), with new-side text", () => {
    const hunk = parseUnifiedDiff(addedDiff(5)).hunks[0];
    const pairs = buildSplitPairs(hunk.lines);
    expect(pairs).toHaveLength(5);
    expect(pairs.every((r) => r.kind === "pair" && r.old === null && r.new?.kind === "add")).toBe(true);
    expect(pairs.map((r) => (r.kind === "pair" ? r.new?.text : null))).toEqual([
      "line 1",
      "line 2",
      "line 3",
      "line 4",
      "line 5",
    ]);
  });

  it("the split grid uses minmax(0, 1fr) so the new column can shrink instead of clipping", () => {
    const css = readFileSync(fileURLToPath(new URL("../styles/diff.css", import.meta.url)), "utf-8");
    expect(css).toContain("minmax(0, 1fr)");
  });
});
