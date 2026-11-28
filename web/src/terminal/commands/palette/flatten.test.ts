import { describe, expect, it } from "vitest";
import { flattenToLine } from "./flatten";

describe("flattenToLine", () => {
  it("collapses LF, CRLF and blank lines to single spaces", () => {
    expect(flattenToLine("a\nb")).toBe("a b");
    expect(flattenToLine("a\r\nb")).toBe("a b");
    expect(flattenToLine("a\n\n  \n b\r\n\r\nc")).toBe("a b c");
  });
  it("trims the ends and leaves a one-line text unchanged", () => {
    expect(flattenToLine("\n a b \n")).toBe("a b");
    expect(flattenToLine("/compact focus on tests")).toBe("/compact focus on tests");
  });
});
