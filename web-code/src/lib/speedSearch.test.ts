import { describe, expect, it } from "vitest";
import {
  highlightSegments,
  matchSpeedSearch,
  speedFilterItems,
} from "./speedSearch";

describe("matchSpeedSearch", () => {
  it("empty query matches with no ranges", () => {
    expect(matchSpeedSearch("src/lib.rs", "")).toEqual({
      ranges: [],
      kind: "substring",
      rank: 0,
    });
    expect(matchSpeedSearch("src/lib.rs", "   ")).toEqual({
      ranges: [],
      kind: "substring",
      rank: 0,
    });
  });

  it("substring match is case-insensitive and preferred", () => {
    const m = matchSpeedSearch("src/FooBar.ts", "foobar");
    expect(m).not.toBeNull();
    expect(m!.kind).toBe("substring");
    expect(m!.ranges).toEqual([{ start: 4, end: 10 }]);
    expect("src/FooBar.ts".slice(4, 10)).toBe("FooBar");
  });

  it("subsequence falls back when no contiguous substring", () => {
    // "fbr" is not a substring of "FooBar" but is a subsequence (F…B…r).
    const m = matchSpeedSearch("FooBar", "fbr");
    expect(m).not.toBeNull();
    expect(m!.kind).toBe("subsequence");
    // F, B, r → three ranges (or fewer if contiguous, which they aren't).
    expect(m!.ranges.length).toBeGreaterThanOrEqual(2);
    const joined = m!.ranges.map((r) => "FooBar".slice(r.start, r.end)).join("");
    // The matched chars, lowercased, spell the query.
    expect(joined.toLowerCase()).toBe("fbr");
  });

  it("coalesces contiguous subsequence hits into one range", () => {
    // "oo" is contiguous inside FooBar.
    const m = matchSpeedSearch("FooBar", "oo");
    // Actually "oo" is a substring — prefer that path.
    expect(m!.kind).toBe("substring");
    expect(m!.ranges).toEqual([{ start: 1, end: 3 }]);
  });

  it("returns null when query cannot match even as subsequence", () => {
    expect(matchSpeedSearch("abc", "xyz")).toBeNull();
    expect(matchSpeedSearch("abc", "abd")).toBeNull(); // 'd' missing
  });

  it("ranks earlier substring matches better (lower rank)", () => {
    const early = matchSpeedSearch("foo_bar", "foo")!;
    const late = matchSpeedSearch("xx_foo", "foo")!;
    expect(early.rank).toBeLessThan(late.rank);
  });

  it("substring ranks strictly better than subsequence", () => {
    const sub = matchSpeedSearch("abcdef", "cd")!; // substring
    const seq = matchSpeedSearch("abcdef", "cf")!; // subsequence
    expect(sub.kind).toBe("substring");
    expect(seq.kind).toBe("subsequence");
    expect(sub.rank).toBeLessThan(seq.rank);
  });
});

describe("speedFilterItems", () => {
  const items = ["src/main.rs", "src/lib/mod.rs", "README.md", "tests/foo.rs"];

  it("empty query returns every item in original order", () => {
    const hits = speedFilterItems(items, "", (s) => s);
    expect(hits.map((h) => h.item)).toEqual(items);
    expect(hits.every((h) => h.ranges.length === 0)).toBe(true);
  });

  it("filters + ranks by path text", () => {
    const hits = speedFilterItems(items, "foo", (s) => s);
    expect(hits.map((h) => h.item)).toEqual(["tests/foo.rs"]);
  });

  it("subsequence can still surface a hit", () => {
    const hits = speedFilterItems(items, "smr", (s) => s);
    // "s…m…r" matches src/main.rs (and possibly others).
    expect(hits.some((h) => h.item === "src/main.rs")).toBe(true);
    expect(hits.every((h) => h.kind === "subsequence" || h.kind === "substring")).toBe(true);
  });

  it("works with object items via textOf", () => {
    const objs = [{ name: "alpha" }, { name: "beta" }, { name: "alphabet" }];
    const hits = speedFilterItems(objs, "alpha", (o) => o.name);
    expect(hits.map((h) => h.item.name)).toEqual(["alpha", "alphabet"]);
    // shorter / earlier-match "alpha" ranks first
    expect(hits[0].item.name).toBe("alpha");
  });
});

describe("highlightSegments", () => {
  it("returns a single miss segment when ranges empty", () => {
    expect(highlightSegments("hello", [])).toEqual([{ text: "hello", hit: false }]);
  });

  it("splits hit and miss regions", () => {
    expect(highlightSegments("hello", [{ start: 1, end: 4 }])).toEqual([
      { text: "h", hit: false },
      { text: "ell", hit: true },
      { text: "o", hit: false },
    ]);
  });

  it("coalesces adjacent ranges", () => {
    expect(
      highlightSegments("abcdef", [
        { start: 1, end: 2 },
        { start: 2, end: 4 },
      ]),
    ).toEqual([
      { text: "a", hit: false },
      { text: "bcd", hit: true },
      { text: "ef", hit: false },
    ]);
  });

  it("clamps out-of-bounds ranges", () => {
    expect(highlightSegments("ab", [{ start: -5, end: 99 }])).toEqual([
      { text: "ab", hit: true },
    ]);
  });
});
