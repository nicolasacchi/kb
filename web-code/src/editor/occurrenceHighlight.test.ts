import { describe, expect, it } from "vitest";
import { findOccurrenceRanges, identifierAt } from "./occurrenceHighlight";

describe("identifierAt", () => {
  it("extracts a plain identifier under the cursor", () => {
    expect(identifierAt("  foo_bar = 1", 2)).toEqual({ word: "foo_bar", start: 2, end: 9 });
    expect(identifierAt("  foo_bar = 1", 5)).toEqual({ word: "foo_bar", start: 2, end: 9 });
  });

  it("prefers the word just before the cursor when sitting on a boundary", () => {
    // Cursor right after the word (col = end).
    expect(identifierAt("foo ", 3)).toEqual({ word: "foo", start: 0, end: 3 });
  });

  it("returns null on bare punctuation / pure whitespace", () => {
    // Cursor on '=' — no word char at or immediately before.
    expect(identifierAt("foo = bar", 4)).toBeNull();
    expect(identifierAt("   ", 1)).toBeNull();
    expect(identifierAt("()", 0)).toBeNull();
  });

  it("falls back to the word before when sitting on the trailing space", () => {
    // Mirrors vimKeys.wordAt: cursor resting just after a word still
    // counts as "on" that word for occurrence highlight.
    expect(identifierAt("foo = bar", 3)).toEqual({ word: "foo", start: 0, end: 3 });
  });

  it("rejects length < 2", () => {
    expect(identifierAt("a = 1", 0)).toBeNull();
    expect(identifierAt("x", 0)).toBeNull();
  });

  it("rejects digit-leading tokens", () => {
    expect(identifierAt("42foo", 0)).toBeNull();
    expect(identifierAt("123", 1)).toBeNull();
  });

  it("treats non-ASCII as a non-word boundary", () => {
    // "café" — only "caf" is word chars; é is a boundary.
    const text = "café";
    const at = identifierAt(text, 0);
    // "caf" is length 3, starts non-digit — accepted if cursor on c/a/f.
    expect(at).toEqual({ word: "caf", start: 0, end: 3 });
    // Cursor on é → no word at/before if we only look at é (non-word) and
    // before is 'f' of "caf" — actually col on é prefers word before.
    const onE = identifierAt(text, 3);
    expect(onE).toEqual({ word: "caf", start: 0, end: 3 });
  });

  it("accepts underscore-leading identifiers (not digit-leading)", () => {
    expect(identifierAt("_foo", 0)).toEqual({ word: "_foo", start: 0, end: 4 });
  });
});

describe("findOccurrenceRanges", () => {
  it("finds every exact-word match", () => {
    const doc = "foo bar foo\nfoo";
    expect(findOccurrenceRanges(doc, "foo")).toEqual([
      { from: 0, to: 3 },
      { from: 8, to: 11 },
      { from: 12, to: 15 },
    ]);
  });

  it("does not match inside a longer word", () => {
    const doc = "foo foobar food foo";
    expect(findOccurrenceRanges(doc, "foo")).toEqual([
      { from: 0, to: 3 },
      { from: 16, to: 19 },
    ]);
  });

  it("does not match a suffix inside a longer word", () => {
    expect(findOccurrenceRanges("afoo foo", "foo")).toEqual([{ from: 5, to: 8 }]);
  });

  it("respects underscore as a word char (boundary)", () => {
    // "foo" must not match inside "foo_bar".
    expect(findOccurrenceRanges("foo foo_bar foo", "foo")).toEqual([
      { from: 0, to: 3 },
      { from: 12, to: 15 },
    ]);
  });

  it("returns empty for short words", () => {
    expect(findOccurrenceRanges("a a a", "a")).toEqual([]);
  });

  it("unicode on either side counts as a boundary", () => {
    // "foo" bounded by é on the left.
    const doc = "éfoo foo";
    expect(findOccurrenceRanges(doc, "foo")).toEqual([
      { from: 1, to: 4 },
      { from: 5, to: 8 },
    ]);
  });
});
