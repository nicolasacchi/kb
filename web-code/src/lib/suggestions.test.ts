import { describe, expect, it } from "vitest";
import type { ReviewComment } from "../api/types";
import {
  formatApplyConflictHint,
  sliceAnchoredLines,
  splitSuggestionLines,
  suggestionIsOutdated,
  synthesizeSuggestionDiff,
  threadAcceptsSuggestion,
} from "./suggestions";

function comment(overrides: Partial<ReviewComment> = {}): ReviewComment {
  return {
    id: "c1",
    path: "src/lib.rs",
    intent: "note",
    body: "look here",
    author: "you",
    created_at: 1000,
    updated_at: 1000,
    resolved: false,
    anchor_kind: "line",
    side: "new",
    ps_number: 1,
    resolution: {
      line: 5,
      orphaned: false,
      resolved_against: { ps: 1, sha: "abc" },
    },
    suggestion: null,
    replies: [],
    ...overrides,
  };
}

describe("splitSuggestionLines", () => {
  it("treats an empty string as zero lines", () => {
    expect(splitSuggestionLines("")).toEqual([]);
  });

  it("drops a single trailing newline", () => {
    expect(splitSuggestionLines("foo\nbar\n")).toEqual(["foo", "bar"]);
  });
});

describe("sliceAnchoredLines", () => {
  const file = "one\ntwo\nthree\nfour\n";

  it("slices a single line when end is omitted", () => {
    expect(sliceAnchoredLines(file, 2)).toEqual(["two"]);
  });

  it("slices an inclusive range", () => {
    expect(sliceAnchoredLines(file, 2, 3)).toEqual(["two", "three"]);
  });

  it("clamps a backwards/missing end to a single line", () => {
    expect(sliceAnchoredLines(file, 3, 1)).toEqual(["three"]);
  });
});

describe("synthesizeSuggestionDiff", () => {
  it("builds one hunk: removes then adds, both numbered from startLine", () => {
    const parsed = synthesizeSuggestionDiff(["old a", "old b"], "new a\nnew b\nnew c", 10);
    expect(parsed.hunks).toHaveLength(1);
    expect(parsed.hunks[0].header).toBe("@@ -10,2 +10,3 @@");
    expect(parsed.hunks[0].oldStart).toBe(10);
    expect(parsed.hunks[0].oldLines).toBe(2);
    expect(parsed.hunks[0].newStart).toBe(10);
    expect(parsed.hunks[0].newLines).toBe(3);
    expect(parsed.hunks[0].lines).toEqual([
      { kind: "remove", text: "old a", oldLine: 10, newLine: null },
      { kind: "remove", text: "old b", oldLine: 11, newLine: null },
      { kind: "add", text: "new a", oldLine: null, newLine: 10 },
      { kind: "add", text: "new b", oldLine: null, newLine: 11 },
      { kind: "add", text: "new c", oldLine: null, newLine: 12 },
    ]);
    expect(parsed.binary).toBe(false);
    expect(parsed.preamble).toEqual([]);
  });

  it("treats an empty replacement as a pure deletion", () => {
    const parsed = synthesizeSuggestionDiff(["gone"], "", 4);
    expect(parsed.hunks[0].header).toBe("@@ -4,1 +4,0 @@");
    expect(parsed.hunks[0].lines).toEqual([
      { kind: "remove", text: "gone", oldLine: 4, newLine: null },
    ]);
    expect(parsed.hunks[0].newLines).toBe(0);
  });

  it("treats an empty original as a pure insertion", () => {
    const parsed = synthesizeSuggestionDiff([], "fresh", 7);
    expect(parsed.hunks[0].header).toBe("@@ -7,0 +7,1 @@");
    expect(parsed.hunks[0].lines).toEqual([
      { kind: "add", text: "fresh", oldLine: null, newLine: 7 },
    ]);
    expect(parsed.hunks[0].oldLines).toBe(0);
  });

  it("handles both sides empty", () => {
    const parsed = synthesizeSuggestionDiff([], "", 1);
    expect(parsed.hunks[0].header).toBe("@@ -1,0 +1,0 @@");
    expect(parsed.hunks[0].lines).toEqual([]);
  });
});

describe("suggestionIsOutdated", () => {
  it("is true only when the resolution is orphaned", () => {
    expect(suggestionIsOutdated(comment())).toBe(false);
    expect(
      suggestionIsOutdated(
        comment({
          resolution: {
            line: null,
            orphaned: true,
            resolved_against: { ps: 1, sha: "abc" },
          },
        }),
      ),
    ).toBe(true);
  });
});

describe("threadAcceptsSuggestion", () => {
  it("accepts a live new-side line thread", () => {
    expect(threadAcceptsSuggestion(comment())).toBe(true);
    expect(threadAcceptsSuggestion(comment({ anchor_kind: "range", side: null }))).toBe(true);
  });

  it("rejects old-side, orphaned, and non-line kinds", () => {
    expect(threadAcceptsSuggestion(comment({ side: "old" }))).toBe(false);
    expect(
      threadAcceptsSuggestion(
        comment({
          resolution: {
            line: null,
            orphaned: true,
            resolved_against: { ps: 1, sha: "abc" },
          },
        }),
      ),
    ).toBe(false);
    expect(threadAcceptsSuggestion(comment({ anchor_kind: "symbol" }))).toBe(false);
  });
});

describe("formatApplyConflictHint", () => {
  it("keeps a short expected-vs-found pair compact", () => {
    expect(
      formatApplyConflictHint({ expected: "foo", found: "bar", resolved_line: 12 }),
    ).toBe('Can\'t apply at L12: expected "foo", found "bar"');
  });

  it("collapses whitespace and truncates a wall of text", () => {
    const long = "alpha ".repeat(20);
    const hint = formatApplyConflictHint({ expected: long, found: "x\ny" });
    expect(hint.length).toBeLessThan(120);
    expect(hint).toContain("expected");
    expect(hint).toContain("found");
    expect(hint).toContain("…");
  });
});
