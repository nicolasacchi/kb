import { describe, expect, it } from "vitest";
import { fromWire, highlightSegments, sliceRanges } from "./matchRanges";

describe("fromWire", () => {
  it("reads the daemon's [start, end] pairs", () => {
    expect(fromWire([[4, 10]])).toEqual([{ start: 4, end: 10 }]);
  });

  it("is total: absent, short or inverted pairs mean NO highlight", () => {
    // An older daemon omits the field entirely; a malformed pair must never
    // become a guessed range (a wrong highlight is a lie about what matched).
    expect(fromWire(undefined)).toEqual([]);
    expect(fromWire(null)).toEqual([]);
    expect(fromWire([[1]])).toEqual([]);
    expect(fromWire([[5, 5]])).toEqual([]);
    expect(fromWire([[7, 2]])).toEqual([]);
  });
});

describe("sliceRanges", () => {
  it("re-bases onto a window and drops what falls outside", () => {
    // `GitRepo::open`, name window = offset 9, length 4.
    const ranges = [
      { start: 0, end: 3 },
      { start: 9, end: 13 },
    ];
    expect(sliceRanges(ranges, 9, 4)).toEqual([{ start: 0, end: 4 }]);
  });

  it("CLIPS a partial overlap rather than dropping it", () => {
    expect(sliceRanges([{ start: 7, end: 12 }], 9, 4)).toEqual([{ start: 0, end: 3 }]);
  });

  it("an empty window yields nothing", () => {
    expect(sliceRanges([{ start: 0, end: 3 }], 0, 0)).toEqual([]);
  });
});

describe("highlightSegments", () => {
  it("splits into hit/miss runs", () => {
    expect(highlightSegments("src/config.rs", [{ start: 4, end: 10 }])).toEqual([
      { text: "src/", hit: false },
      { text: "config", hit: true },
      { text: ".rs", hit: false },
    ]);
  });

  it("no ranges renders one plain run", () => {
    expect(highlightSegments("abc", [])).toEqual([{ text: "abc", hit: false }]);
  });

  it("UTF-16 offsets from the daemon land on the right characters", () => {
    // The daemon converts nucleo's CHAR indices to UTF-16 code units for
    // exactly this reason — a surrogate pair is two units here.
    const text = "src/🦀/config.rs";
    const marked = highlightSegments(text, [{ start: 7, end: 13 }])
      .filter((s) => s.hit)
      .map((s) => s.text)
      .join("");
    expect(marked).toBe("config");
  });
});
