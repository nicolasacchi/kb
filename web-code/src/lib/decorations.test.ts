import { describe, expect, it } from "vitest";
import { cssClassFor, makeByteToUtf16Mapper, spansToDecorationRanges } from "./decorations";
import type { Span } from "../api/types";

describe("makeByteToUtf16Mapper", () => {
  it("is the identity for pure-ASCII content", () => {
    const content = "fn main() {\n    println!(\"hi\");\n}\n";
    const map = makeByteToUtf16Mapper(content);
    for (let i = 0; i <= content.length; i++) {
      expect(map(i)).toBe(i);
    }
  });

  it("maps a byte offset past a multi-byte character correctly", () => {
    // "é" is U+00E9 — 2 UTF-8 bytes, 1 UTF-16 code unit.
    const content = "é = 1";
    // bytes: [0xC3, 0xA9, ' ', '=', ' ', '1'] — "1" starts at byte 6.
    const map = makeByteToUtf16Mapper(content);
    expect(map(0)).toBe(0); // before "é"
    expect(map(2)).toBe(1); // right after "é" (2 bytes) → utf16 index 1
    expect(map(6)).toBe(5); // "1" — utf16 index 5 (é,' ','=',' ' = 4 chars + 1)
  });

  it("handles an astral character (surrogate pair) as one code point", () => {
    // U+1F600 (😀) — 4 UTF-8 bytes, 2 UTF-16 code units (a surrogate pair).
    const content = "😀x";
    const map = makeByteToUtf16Mapper(content);
    expect(content.length).toBe(3); // 2 surrogate units + "x"
    expect(map(0)).toBe(0);
    expect(map(4)).toBe(2); // right after the emoji → utf16 index 2 ("x")
    expect(map(5)).toBe(3); // right after "x"
  });

  it("clamps below zero and above the content's total byte length", () => {
    const content = "abc";
    const map = makeByteToUtf16Mapper(content);
    expect(map(-5)).toBe(0);
    expect(map(1000)).toBe(3);
  });

  it("returns the same mapper results whether content is empty", () => {
    const map = makeByteToUtf16Mapper("");
    expect(map(0)).toBe(0);
    expect(map(5)).toBe(0);
  });
});

describe("spansToDecorationRanges", () => {
  const content = "fn main() {}\n";

  it("maps ascii spans through unchanged", () => {
    const spans: Span[] = [
      { byte_start: 0, byte_len: 2, class: "keyword" },
      { byte_start: 3, byte_len: 4, class: "function" },
    ];
    const ranges = spansToDecorationRanges(content, spans);
    expect(ranges).toEqual([
      { from: 0, to: 2, class: "keyword" },
      { from: 3, to: 7, class: "function" },
    ]);
  });

  it("drops a span that becomes zero-width after mapping", () => {
    const spans: Span[] = [{ byte_start: 5, byte_len: 0, class: "punctuation" }];
    const ranges = spansToDecorationRanges(content, spans);
    expect(ranges).toEqual([]);
  });

  it("clamps a span whose end exceeds the content length", () => {
    const spans: Span[] = [{ byte_start: 10, byte_len: 1000, class: "string" }];
    const ranges = spansToDecorationRanges(content, spans);
    expect(ranges).toEqual([{ from: 10, to: content.length, class: "string" }]);
  });

  it("preserves multi-byte content offsets end to end", () => {
    const multi = "// café\nfn x() {}\n";
    // "café" — the é is bytes 8-9 in the comment; "fn" starts after the
    // newline. Just assert the "fn" keyword span survives the round trip
    // at the correct (smaller) utf16 offset despite the wider byte offset.
    const byteOfFn = new TextEncoder().encode(multi).findIndex(
      (_, idx, arr) => arr[idx] === 0x66 && arr[idx + 1] === 0x6e,
    );
    const spans: Span[] = [{ byte_start: byteOfFn, byte_len: 2, class: "keyword" }];
    const ranges = spansToDecorationRanges(multi, spans);
    expect(ranges).toHaveLength(1);
    expect(multi.slice(ranges[0].from, ranges[0].to)).toBe("fn");
  });

  it("returns an empty array for an empty span list", () => {
    expect(spansToDecorationRanges(content, [])).toEqual([]);
  });
});

describe("cssClassFor", () => {
  it("prefixes every highlight class with kbc-hl-", () => {
    expect(cssClassFor("keyword")).toBe("kbc-hl-keyword");
    expect(cssClassFor("other")).toBe("kbc-hl-other");
    // V72-H2b — the three widened roles arrive kebab-cased on the wire and
    // pass straight through.
    expect(cssClassFor("string-special")).toBe("kbc-hl-string-special");
    expect(cssClassFor("constant-builtin")).toBe("kbc-hl-constant-builtin");
    expect(cssClassFor("punctuation-special")).toBe("kbc-hl-punctuation-special");
  });
});
