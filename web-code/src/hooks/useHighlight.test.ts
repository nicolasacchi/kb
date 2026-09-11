import { describe, expect, it } from "vitest";
import { highlightCacheKey, uniqueHighlightItems, type HighlightItem } from "./useHighlight";

describe("highlightCacheKey", () => {
  it("is stable for the same (lang, text)", () => {
    expect(highlightCacheKey("ruby", "def x\nend\n")).toBe(highlightCacheKey("ruby", "def x\nend\n"));
  });

  it("differs when lang or text differs", () => {
    const a = highlightCacheKey("ruby", "a");
    const b = highlightCacheKey("rust", "a");
    const c = highlightCacheKey("ruby", "b");
    expect(a).not.toBe(b);
    expect(a).not.toBe(c);
  });

  it("path only participates when lang is null", () => {
    expect(highlightCacheKey("ruby", "a", "x.rb")).toBe(highlightCacheKey("ruby", "a", "y.rs"));
    expect(highlightCacheKey(null, "a", "x.rb")).not.toBe(highlightCacheKey(null, "a", "y.rs"));
  });
});

describe("uniqueHighlightItems — batching", () => {
  it("collapses duplicate (lang,text) into one batch payload", () => {
    const items: HighlightItem[] = [
      { id: "a", lang: "ruby", text: "def x\nend\n" },
      { id: "b", lang: "ruby", text: "def x\nend\n" },
      { id: "c", lang: "rust", text: "fn x() {}\n" },
    ];
    const unique = uniqueHighlightItems(items);
    expect(unique).toHaveLength(2);
    expect(unique.map((u) => u.lang).sort()).toEqual(["ruby", "rust"]);
  });

  it("drops empty text so a spinner never waits on nothing", () => {
    expect(uniqueHighlightItems([{ id: "a", lang: "ruby", text: "" }])).toEqual([]);
  });
});
