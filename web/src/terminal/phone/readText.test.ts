// T17 — Read-view text from an xterm-shaped buffer.

import { describe, expect, it } from "vitest";
import {
  bufferToCopyText,
  bufferToLines,
  centerScrollTop,
  isImageLine,
  linkParts,
  snapshotLines,
  type ReadBuffer,
} from "./readText";

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

function wrapFake(rows: Array<[string, boolean]>): ReadBuffer {
  return {
    length: rows.length,
    getLine: (y) => {
      const r = rows[y];
      if (!r) return undefined;
      return { isWrapped: r[1], translateToString: (trim?: boolean) => (trim ? r[0].replace(/\s+$/, "") : r[0]) };
    },
  };
}

describe("FUPHONELINKS bufferToCopyText", () => {
  it("joins wrapped continuation rows, keeping the spaces at the wrap boundary", () => {
    const b = wrapFake([
      ["see https://exam", false],
      ["ple.com/p and a  ", true],
      ["wrapped tail", true],
      ["next line", false],
      ["", false],
    ]);
    expect(bufferToCopyText(b)).toBe("see https://example.com/p and a  wrapped tail\nnext line");
  });

  it("keeps only the last `max` logical lines", () => {
    const b = wrapFake([
      ["a", false],
      ["b", false],
      ["c1", false],
      ["c2", true],
    ]);
    expect(bufferToCopyText(b, { max: 2 })).toBe("b\nc1c2");
  });
});

describe("FUPHONELINKS snapshotLines", () => {
  it("reports how many leading lines the max cut dropped", () => {
    const b = fake(["1", "2", "3", "4", "5", ""]);
    expect(snapshotLines(b, { max: 3 })).toEqual({ lines: ["3", "4", "5"], dropped: 2 });
    expect(snapshotLines(b, {})).toEqual({ lines: ["1", "2", "3", "4", "5"], dropped: 0 });
  });
});

describe("FUPHONELINKS linkParts", () => {
  it("splits a line into text and http(s) urls, trimming sentence punctuation", () => {
    expect(linkParts("go to https://a.dev/x. then http://b.dev/y)")).toEqual([
      "go to ",
      { url: "https://a.dev/x" },
      ". then ",
      { url: "http://b.dev/y" },
      ")",
    ]);
    expect(linkParts("no links, ftp://x and javascript:alert(1)")).toEqual(["no links, ftp://x and javascript:alert(1)"]);
  });
});

describe("FUPHONELINKS centerScrollTop", () => {
  it("puts the middle of the line at the middle of the view, clamped to the scroll range", () => {
    const m = { lines: 100, contentHeight: 2000, viewHeight: 200, pad: 0 };
    expect(centerScrollTop({ ...m, line: 50 })).toBe(50 * 20 + 10 - 100);
    expect(centerScrollTop({ ...m, line: 0 })).toBe(0);
    expect(centerScrollTop({ ...m, line: 99 })).toBe(1800);
    // The padding is not a line: 100 lines of 20 px between two 10 px pads.
    expect(centerScrollTop({ line: 50, lines: 100, contentHeight: 2020, viewHeight: 200, pad: 10 })).toBe(
      10 + 50 * 20 + 10 - 100,
    );
    expect(centerScrollTop({ ...m, lines: 0, line: 0 })).toBe(0);
  });
});
