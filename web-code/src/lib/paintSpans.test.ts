import { describe, expect, it } from "vitest";
import type { HighlightSpan, Span } from "../api/types";
import {
  byteSpansToHighlightSpans,
  classSequence,
  offsetHighlightSpans,
  padSnippetLines,
  paintSpans,
} from "./paintSpans";

function hs(
  line: number,
  start: number,
  end: number,
  role: HighlightSpan["role"] = "keyword",
): HighlightSpan {
  return { line, start, end, role };
}

describe("paintSpans", () => {
  it("paints a ruby-shaped line into the reader's kbc-hl-* classes", () => {
    const text = "def greet(name)\nend\n";
    const spans: HighlightSpan[] = [
      hs(1, 0, 3, "keyword"),
      hs(1, 4, 9, "function"),
      hs(2, 0, 3, "keyword"),
    ];
    expect(classSequence(text, spans)).toEqual([
      "kbc-hl-keyword:def",
      ": ",
      "kbc-hl-function:greet",
      ":(name)",
      "kbc-hl-keyword:end",
    ]);
  });

  it("empty spans yield one unclassed segment per line", () => {
    expect(paintSpans("a\nb", [])).toEqual([[{ text: "a" }], [{ text: "b" }]]);
  });

  it("a none-tier (empty) span list never invents a class", () => {
    const seq = classSequence("SELECT 1;", []);
    expect(seq).toEqual([":SELECT 1;"]);
    expect(seq.some((s) => s.startsWith("kbc-hl-"))).toBe(false);
  });
});

describe("byteSpansToHighlightSpans", () => {
  it("buckets a file-style byte span onto line 1", () => {
    const content = "fn main() {\n    x\n}\n";
    const spans: Span[] = [{ byte_start: 0, byte_len: 2, class: "keyword" }];
    expect(byteSpansToHighlightSpans(content, spans)).toEqual([
      { line: 1, start: 0, end: 2, role: "keyword" },
    ]);
  });

  it("splits a multi-line byte span at the newline", () => {
    const content = "ab\ncd\n";
    const spans: Span[] = [{ byte_start: 1, byte_len: 3, class: "comment" }];
    expect(byteSpansToHighlightSpans(content, spans)).toEqual([
      { line: 1, start: 1, end: 2, role: "comment" },
      { line: 2, start: 0, end: 1, role: "comment" },
    ]);
  });
});

describe("offsetHighlightSpans / padSnippetLines", () => {
  it("shifts snippet lines onto a file line base", () => {
    expect(offsetHighlightSpans([hs(1, 0, 3)], 40)).toEqual([hs(40, 0, 3)]);
  });

  it("pads so lines[n-1] is file line n", () => {
    expect(padSnippetLines("a\nb", 3)).toEqual(["", "", "a", "b"]);
  });
});
