import { describe, expect, it } from "vitest";
import { diffLines } from "./promptDiff";

describe("diffLines", () => {
  it("identical text is all equal", () => {
    const t = "a\nb\nc";
    expect(diffLines(t, t)).toEqual([
      { tag: "equal", text: "a" },
      { tag: "equal", text: "b" },
      { tag: "equal", text: "c" },
    ]);
  });

  it("disjoint text is every delete then every insert", () => {
    expect(diffLines("a\nb", "x\ny")).toEqual([
      { tag: "delete", text: "a" },
      { tag: "delete", text: "b" },
      { tag: "insert", text: "x" },
      { tag: "insert", text: "y" },
    ]);
  });

  it("keeps a common prefix and suffix, diffing only the middle", () => {
    expect(diffLines("a\nb\nOLD\nc\nd", "a\nb\nNEW\nc\nd")).toEqual([
      { tag: "equal", text: "a" },
      { tag: "equal", text: "b" },
      { tag: "delete", text: "OLD" },
      { tag: "insert", text: "NEW" },
      { tag: "equal", text: "c" },
      { tag: "equal", text: "d" },
    ]);
  });

  it("empty old side is all inserts", () => {
    expect(diffLines("", "a\nb")).toEqual([
      { tag: "insert", text: "a" },
      { tag: "insert", text: "b" },
    ]);
  });

  it("empty new side is all deletes", () => {
    expect(diffLines("a\nb", "")).toEqual([
      { tag: "delete", text: "a" },
      { tag: "delete", text: "b" },
    ]);
  });

  it("both sides empty is empty", () => {
    expect(diffLines("", "")).toEqual([]);
  });

  it("trims a single trailing newline (not a phantom empty line)", () => {
    expect(diffLines("a\nb\n", "a\nb\n")).toEqual([
      { tag: "equal", text: "a" },
      { tag: "equal", text: "b" },
    ]);
  });
});
