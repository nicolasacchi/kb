import { describe, expect, it } from "vitest";
import { parseUnifiedDiff } from "./diff";
import {
  combineExpand,
  contextCaption,
  ctxLinesFor,
  dialNeedsFile,
  EXPAND_STEP,
  expandForDial,
  expandHunk,
  expandParsed,
  fileLines,
} from "./diffContext";

/// A NON-size-neutral hunk: it removes one line and adds two, so the
/// above-delta and the below-delta differ. This is the case that catches
/// the "one delta for both sides" simplification the module header warns
/// about.
const DIFF = `@@ -20,5 +30,6 @@ fn a() {
 ctx a
 ctx b
-gone
+new one
+new two
 ctx c
 ctx d
`;

/// The NEW-side file this hunk came from: 40 lines, where lines 30..35 are
/// exactly the hunk's own rendered window.
const FILE = Array.from({ length: 40 }, (_, i) => {
  const n = i + 1;
  if (n === 30) return "ctx a";
  if (n === 31) return "ctx b";
  if (n === 32) return "new one";
  if (n === 33) return "new two";
  if (n === 34) return "ctx c";
  if (n === 35) return "ctx d";
  return `file line ${n}`;
}).join("\n");

const hunk = parseUnifiedDiff(DIFF).hunks[0];
const lines = fileLines(FILE);

describe("fileLines", () => {
  it("numbers 1-based and drops the trailing-newline phantom", () => {
    expect(fileLines("a\nb\n")).toEqual(["a", "b"]);
    expect(fileLines("a\nb")).toEqual(["a", "b"]);
    expect(fileLines("")).toEqual([]);
  });
});

describe("the dial", () => {
  it("3 is git's own -U3 and needs no file fetch", () => {
    expect(ctxLinesFor(3)).toBe(3);
    expect(dialNeedsFile(3)).toBe(false);
    expect(expandForDial(3)).toEqual({ before: 0, after: 0 });
  });

  it("10 asks for seven more each side", () => {
    expect(expandForDial(10)).toEqual({ before: 7, after: 7 });
    expect(dialNeedsFile(10)).toBe(true);
  });

  it("full asks for everything and needs the file", () => {
    expect(expandForDial("full")).toEqual({
      before: Number.POSITIVE_INFINITY,
      after: Number.POSITIVE_INFINITY,
    });
    expect(dialNeedsFile("full")).toBe(true);
  });

  it("manual expands ADD to the dial's own width", () => {
    expect(combineExpand(10, { before: EXPAND_STEP, after: 0 })).toEqual({
      before: 7 + EXPAND_STEP,
      after: 7,
    });
    expect(combineExpand("full", { before: 10, after: 10 })).toEqual({
      before: Number.POSITIVE_INFINITY,
      after: Number.POSITIVE_INFINITY,
    });
  });
});

describe("expandHunk", () => {
  it("is a no-op at the dial's default", () => {
    const out = expandHunk(hunk, lines, { before: 0, after: 0 });
    expect(out.lines).toEqual(hunk.lines);
    expect(out.addedBefore).toBe(0);
    expect(out.addedAfter).toBe(0);
    expect(out.moreAbove).toBe(true);
    expect(out.moreBelow).toBe(true);
  });

  it("splices REAL file rows, above and below", () => {
    const out = expandHunk(hunk, lines, { before: 2, after: 2 });
    expect(out.addedBefore).toBe(2);
    expect(out.addedAfter).toBe(2);
    expect(out.lines.slice(0, 2).map((l) => l.text)).toEqual(["file line 28", "file line 29"]);
    expect(out.lines.slice(-2).map((l) => l.text)).toEqual(["file line 36", "file line 37"]);
    // Nothing was invented: every spliced row is the file's own text.
    for (const l of [...out.lines.slice(0, 2), ...out.lines.slice(-2)]) {
      expect(l.kind).toBe("context");
      expect(l.newLine).not.toBeNull();
      expect(lines[(l.newLine as number) - 1]).toBe(l.text);
    }
  });

  it("uses the ABOVE delta above and the BELOW delta below (the non-size-neutral case)", () => {
    // above: new 30 ↔ old 20 ⇒ delta 10. below: new 36 ↔ old 25 ⇒ delta 11.
    const out = expandHunk(hunk, lines, { before: 2, after: 2 });
    expect(out.lines[0]).toMatchObject({ newLine: 28, oldLine: 18 });
    expect(out.lines[out.lines.length - 1]).toMatchObject({ newLine: 37, oldLine: 26 });
  });

  it("clamps at the file's own bounds and reports there is no more", () => {
    const out = expandHunk(hunk, lines, { before: 999, after: 999 });
    expect(out.addedBefore).toBe(29);
    expect(out.addedAfter).toBe(5);
    expect(out.moreAbove).toBe(false);
    expect(out.moreBelow).toBe(false);
  });

  it("full spans the whole file", () => {
    const out = expandHunk(hunk, lines, expandForDial("full"));
    expect(out.lines[0].newLine).toBe(1);
    expect(out.lines[out.lines.length - 1].newLine).toBe(40);
  });

  it("NEVER fabricates when the file is unavailable — the hunk stays at wire width", () => {
    const out = expandHunk(hunk, null, { before: 10, after: 10 });
    expect(out.lines).toEqual(hunk.lines);
    expect(out.addedBefore).toBe(0);
    expect(out.addedAfter).toBe(0);
    // It still says there IS more, which is true and is what keeps the
    // expand affordance honest rather than silently disappearing.
    expect(out.moreAbove).toBe(true);
    expect(out.moreBelow).toBe(true);
  });

  it("a pure-deletion hunk has no new-side anchor and is left alone", () => {
    const del = parseUnifiedDiff("@@ -5,2 +4,0 @@\n-a\n-b\n").hunks[0];
    const out = expandHunk(del, lines, { before: 5, after: 5 });
    expect(out.lines).toEqual(del.lines);
    expect(out.moreAbove).toBe(false);
    expect(out.moreBelow).toBe(false);
  });
});

describe("contextCaption", () => {
  it("says nothing at the default width", () => {
    expect(contextCaption(3, false, false)).toBeNull();
    expect(contextCaption(3, false, true)).toBeNull();
  });

  it("says nothing once the file is in hand", () => {
    expect(contextCaption(10, true, false)).toBeNull();
  });

  it("names the fetch, then names the failure — never a silent narrow view", () => {
    expect(contextCaption("full", false, true)).toContain("fetching the file");
    expect(contextCaption("full", false, false)).toContain("content is unavailable");
  });
});

describe("expandParsed", () => {
  it("expands every hunk, with per-hunk manual amounts", () => {
    const parsed = parseUnifiedDiff(DIFF);
    const out = expandParsed(parsed, lines, 3, new Map([[0, { before: 1, after: 0 }]]));
    expect(out).toHaveLength(1);
    expect(out[0].addedBefore).toBe(1);
    expect(out[0].addedAfter).toBe(0);
  });
});
