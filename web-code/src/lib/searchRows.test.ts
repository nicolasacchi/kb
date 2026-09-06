import { describe, expect, it } from "vitest";
import type { TextFileResult } from "../api/types";
import { flattenTextRows, formatSessionDate, highlightMatch } from "./searchRows";

describe("flattenTextRows", () => {
  it("emits one row per match, preserving file then match order", () => {
    const results: TextFileResult[] = [
      {
        path: "a.rs",
        matches: [
          { line_no: 1, line: "fn a() {}", byte_range: [0, 2] },
          { line_no: 5, line: "fn a2() {}", byte_range: [0, 2] },
        ],
      },
      { path: "b.rs", matches: [{ line_no: 3, line: "fn b() {}", byte_range: [0, 2] }] },
    ];
    expect(flattenTextRows(results).map((r) => [r.path, r.line_no])).toEqual([
      ["a.rs", 1],
      ["a.rs", 5],
      ["b.rs", 3],
    ]);
  });

  it("is empty for a file with no matches and for an empty result list", () => {
    expect(flattenTextRows([{ path: "empty.rs", matches: [] }])).toEqual([]);
    expect(flattenTextRows([])).toEqual([]);
  });
});

describe("highlightMatch", () => {
  it("splits an ASCII line into before/match/after", () => {
    const line = "fn add(a, b) {}";
    // "add" starts at byte 3, length 3.
    expect(highlightMatch(line, [3, 6])).toEqual({ before: "fn ", match: "add", after: "(a, b) {}" });
  });

  it("maps byte offsets past a multi-byte character correctly", () => {
    // "café " is 6 UTF-8 bytes (c=1,a=1,f=1,é=2,space=1) but 5 UTF-16 code
    // units - "fn" starts at byte offset 6, UTF-16 index 5.
    const line = "café fn";
    expect(highlightMatch(line, [6, 8])).toEqual({ before: "café ", match: "fn", after: "" });
  });

  it("returns the whole line as `before` when the range is empty/invalid", () => {
    expect(highlightMatch("hello", [2, 2])).toEqual({ before: "hello", match: "", after: "" });
  });
});

describe("formatSessionDate", () => {
  it("formats a unix-seconds timestamp as YYYY-MM-DD in UTC", () => {
    // 2026-07-17T00:00:00Z
    expect(formatSessionDate(1784246400)).toBe("2026-07-17");
  });
});
