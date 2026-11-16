// T17 — Read-view text from an xterm-shaped buffer.

import { describe, expect, it } from "vitest";
import { bufferToLines, isImageLine, type ReadBuffer } from "./readText";

function fake(rows: string[]): ReadBuffer & { reads: number[] } {
  const reads: number[] = [];
  return {
    length: rows.length,
    reads,
    getLine: (y) => {
      reads.push(y);
      const text = rows[y];
      if (text === undefined) return undefined;
      return { translateToString: (trim?: boolean) => (trim ? text.replace(/\s+$/, "") : text) };
    },
  };
}

describe("bufferToLines", () => {
  it("keeps order, right-trims each line and drops the trailing blank pair", () => {
    const b = fake(["alpha   ", "beta", "gamma  ", "", ""]);
    expect(bufferToLines(b)).toEqual(["alpha", "beta", "gamma"]);
  });

  it("does not join a wrapped line: each hard-broken row stays its own line", () => {
    const b = fake(["aaaaaaaaaa", "bbbb", "next"]);
    expect(bufferToLines(b)).toEqual(["aaaaaaaaaa", "bbbb", "next"]);
  });

  it("keeps interior blank lines and a missing row reads as blank", () => {
    const b = fake(["one", "", "three"]);
    expect(bufferToLines(b)).toEqual(["one", "", "three"]);
  });

  it("keeps only the last `max` lines", () => {
    const b = fake(["1", "2", "3", "4", "5"]);
    expect(bufferToLines(b, { max: 2 })).toEqual(["4", "5"]);
    expect(bufferToLines(b, { max: 9 })).toEqual(["1", "2", "3", "4", "5"]);
  });

  it("reads every row of the buffer it is given, from 0 to length", () => {
    const b = fake(["a", "b", "c"]);
    bufferToLines(b);
    expect(b.reads).toEqual([0, 1, 2]);
  });
});

describe("isImageLine", () => {
  it("is true for a line that starts with a placeholder cell", () => {
    expect(isImageLine("\u{10EEEE}\u0301\u{10EEEE}")).toBe(true);
    expect(isImageLine("plain \u{10EEEE}")).toBe(false);
    expect(isImageLine("")).toBe(false);
  });
});
