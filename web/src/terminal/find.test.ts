import { describe, expect, it } from "vitest";
import { countHits, findNext, type FindLineSource } from "./find";

function source(lines: string[]): FindLineSource {
  return {
    length: lines.length,
    lineToString: (y) => lines[y] ?? "",
  };
}

describe("findNext", () => {
  it("finds forward and wraps", () => {
    const s = source(["foo bar", "baz foo", "nope"]);
    expect(findNext(s, "foo", { line: 0, col: -1, length: 0 }, 1)).toEqual({ line: 0, col: 0, length: 3 });
    expect(findNext(s, "foo", { line: 0, col: 0, length: 3 }, 1)).toEqual({ line: 1, col: 4, length: 3 });
    expect(findNext(s, "foo", { line: 1, col: 4, length: 3 }, 1)).toEqual({ line: 0, col: 0, length: 3 });
  });

  it("finds backward and wraps", () => {
    const s = source(["foo bar", "baz foo", "nope"]);
    expect(findNext(s, "foo", { line: 1, col: 4, length: 3 }, -1)).toEqual({ line: 0, col: 0, length: 3 });
    expect(findNext(s, "foo", { line: 0, col: 0, length: 3 }, -1)).toEqual({ line: 1, col: 4, length: 3 });
  });

  it("single line: a match before/after the cursor is reachable only via full wrap", () => {
    const s = source(["foo bar foo"]);
    // Forward from the FIRST hit must wrap back to the line start and
    // find the trailing... no: from the first hit the only other hit is
    // after it on the same line (step 0). From the LAST hit a forward
    // search can only return after wrapping, re-scanning from col 0.
    expect(findNext(s, "foo", { line: 0, col: 0, length: 3 }, 1)).toEqual({ line: 0, col: 8, length: 3 });
    expect(findNext(s, "foo", { line: 0, col: 8, length: 3 }, 1)).toEqual({ line: 0, col: 0, length: 3 });
    expect(findNext(s, "foo", { line: 0, col: 8, length: 3 }, -1)).toEqual({ line: 0, col: 0, length: 3 });
    expect(findNext(s, "foo", { line: 0, col: 0, length: 3 }, -1)).toEqual({ line: 0, col: 8, length: 3 });
  });

  it("returns null for empty query or no match", () => {
    const s = source(["abc"]);
    expect(findNext(s, "", { line: 0, col: 0, length: 0 }, 1)).toBeNull();
    expect(findNext(s, "zz", { line: 0, col: 0, length: 0 }, 1)).toBeNull();
  });

  it("countHits counts all occurrences", () => {
    expect(countHits(source(["foo foo", "foo", "bar"]), "foo")).toBe(3);
    expect(countHits(source(["foo foo", "foo", "bar"]), "")).toBe(0);
  });
});
