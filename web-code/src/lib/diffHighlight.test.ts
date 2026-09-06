import { describe, expect, it } from "vitest";
import type { ServerSpan } from "./diffHighlight";
import {
  buildLineSpans,
  paintLine,
  spansForLine,
  splitContentLines,
  type DiffHighlights,
} from "./diffHighlight";

function span(byte_start: number, byte_len: number, cls: ServerSpan["class"] = "keyword"): ServerSpan {
  return { byte_start, byte_len, class: cls };
}

describe("splitContentLines", () => {
  it("drops a single trailing empty element from a final newline", () => {
    expect(splitContentLines("a\nb\n")).toEqual(["a", "b"]);
  });

  it("keeps the last line when there is no trailing newline", () => {
    expect(splitContentLines("a\nb")).toEqual(["a", "b"]);
  });

  it("returns no lines for an empty file", () => {
    expect(splitContentLines("")).toEqual([]);
  });
});

describe("buildLineSpans — ascii", () => {
  const content = "fn main() {\n    x\n}\n";

  it("buckets a keyword on line 1", () => {
    const map = buildLineSpans(content, [span(0, 2, "keyword")]);
    expect(map.get(1)).toEqual([{ start: 0, end: 2, cls: "kbc-hl-keyword" }]);
    expect(map.has(2)).toBe(false);
  });

  it("buckets a span on a later ascii line", () => {
    // "    x" starts after "fn main() {\n" (12 bytes).
    const map = buildLineSpans(content, [span(16, 1, "variable")]);
    expect(map.get(2)).toEqual([{ start: 4, end: 5, cls: "kbc-hl-variable" }]);
  });
});

describe("buildLineSpans — multi-byte UTF-8", () => {
  it("maps é (2 UTF-8 bytes, 1 UTF-16 unit) to a one-column span", () => {
    // "é = 1\n" — é is bytes 0-1.
    const content = "é = 1\n";
    const map = buildLineSpans(content, [span(0, 2, "variable")]);
    expect(map.get(1)).toEqual([{ start: 0, end: 1, cls: "kbc-hl-variable" }]);
    expect(paintLine("é = 1", map.get(1)).map((s) => s.text).join("")).toBe("é = 1");
  });

  it("maps an emoji (4 UTF-8 bytes, 2 UTF-16 units) to a two-column span", () => {
    // "😀x\n" — emoji is bytes 0-3, "x" is byte 4.
    const content = "😀x\n";
    const map = buildLineSpans(content, [span(0, 4, "string"), span(4, 1, "variable")]);
    expect(map.get(1)).toEqual([
      { start: 0, end: 2, cls: "kbc-hl-string" },
      { start: 2, end: 3, cls: "kbc-hl-variable" },
    ]);
    const segs = paintLine("😀x", map.get(1));
    expect(segs).toEqual([
      { text: "😀", cls: "kbc-hl-string" },
      { text: "x", cls: "kbc-hl-variable" },
    ]);
  });
});

describe("buildLineSpans — span crossing a newline", () => {
  it("splits a multi-line span at the line boundary", () => {
    const content = "ab\ncd\n";
    // bytes: a=0 b=1 \n=2 c=3 d=4 \n=5 — cover b + \n + c
    const map = buildLineSpans(content, [span(1, 3, "comment")]);
    expect(map.get(1)).toEqual([{ start: 1, end: 2, cls: "kbc-hl-comment" }]);
    expect(map.get(2)).toEqual([{ start: 0, end: 1, cls: "kbc-hl-comment" }]);
  });
});

describe("buildLineSpans — span at EOF", () => {
  it("clamps a span that runs past the last byte", () => {
    const content = "ab";
    const map = buildLineSpans(content, [span(1, 100, "string")]);
    expect(map.get(1)).toEqual([{ start: 1, end: 2, cls: "kbc-hl-string" }]);
  });

  it("drops a zero-length span at EOF", () => {
    const content = "ab";
    expect(buildLineSpans(content, [span(2, 0, "string")]).size).toBe(0);
  });
});

describe("buildLineSpans — empty file", () => {
  it("returns an empty map", () => {
    expect(buildLineSpans("", [span(0, 1, "keyword")]).size).toBe(0);
    expect(buildLineSpans("", []).size).toBe(0);
  });
});

describe("buildLineSpans — overlapping spans", () => {
  // Last-wins is applied at paint time (see paintLine). buildLineSpans
  // keeps both spans; the later array entry overwrites shared columns.
  it("keeps both spans so paintLine can last-win", () => {
    const content = "abcdef";
    const map = buildLineSpans(content, [span(0, 4, "keyword"), span(2, 4, "string")]);
    expect(map.get(1)).toEqual([
      { start: 0, end: 4, cls: "kbc-hl-keyword" },
      { start: 2, end: 6, cls: "kbc-hl-string" },
    ]);
    expect(paintLine("abcdef", map.get(1))).toEqual([
      { text: "ab", cls: "kbc-hl-keyword" },
      { text: "cdef", cls: "kbc-hl-string" },
    ]);
  });
});

describe("paintLine", () => {
  it("returns a single unclassed segment when spans are missing", () => {
    expect(paintLine("hello", undefined)).toEqual([{ text: "hello" }]);
    expect(paintLine("hello", [])).toEqual([{ text: "hello" }]);
  });

  it("splits a line into classed and unclassed runs", () => {
    expect(
      paintLine("fn x", [
        { start: 0, end: 2, cls: "kbc-hl-keyword" },
        { start: 3, end: 4, cls: "kbc-hl-variable" },
      ]),
    ).toEqual([
      { text: "fn", cls: "kbc-hl-keyword" },
      { text: " " },
      { text: "x", cls: "kbc-hl-variable" },
    ]);
  });

  it("clamps out-of-bounds columns", () => {
    expect(paintLine("ab", [{ start: -2, end: 99, cls: "kbc-hl-other" }])).toEqual([
      { text: "ab", cls: "kbc-hl-other" },
    ]);
  });
});

describe("spansForLine — integrity guard", () => {
  const highlights: DiffHighlights = {
    oldLineSpans: new Map([[1, [{ start: 0, end: 2, cls: "kbc-hl-keyword" }]]]),
    newLineSpans: new Map([[2, [{ start: 0, end: 2, cls: "kbc-hl-keyword" }]]]),
    oldLines: ["fn"],
    newLines: ["aa", "fn"],
  };

  it("returns spans when the file line matches", () => {
    expect(spansForLine(highlights, "old", 1, "fn")).toEqual([
      { start: 0, end: 2, cls: "kbc-hl-keyword" },
    ]);
    expect(spansForLine(highlights, "new", 2, "fn")).toHaveLength(1);
  });

  it("returns undefined on text mismatch (CRLF / wrong-sha)", () => {
    expect(spansForLine(highlights, "old", 1, "fn\r")).toBeUndefined();
    expect(spansForLine(highlights, "new", 2, "FN")).toBeUndefined();
  });

  it("returns undefined when that side has no lines", () => {
    expect(
      spansForLine({ ...highlights, oldLines: null }, "old", 1, "fn"),
    ).toBeUndefined();
  });
});
