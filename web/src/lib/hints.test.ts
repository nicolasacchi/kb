import { describe, expect, test } from "vitest";
import { labelFor, labelsFor } from "./hints";

describe("labelFor — golden-pinned home-row-first sequence", () => {
  test("the first 26 labels are the fixed single-key alphabet, in order", () => {
    expect(labelsFor(26)).toEqual([
      "a", "s", "d", "f", "g", "h", "j", "k", "l",
      "q", "w", "e", "r", "t", "y", "u", "i", "o", "p",
      "z", "x", "c", "v", "b", "n", "m",
    ]);
  });

  test("every single-key label is exactly one character", () => {
    for (const label of labelsFor(26)) expect(label).toHaveLength(1);
  });

  test("the 27th label (index 26) is the first two-key combination", () => {
    expect(labelFor(26)).toBe("aa");
  });

  test("two-key labels are deterministic and stay two characters", () => {
    expect(labelFor(27)).toBe("as"); // 26 (base) + 1 → row 0, col 1 → "a" + "s"
    expect(labelFor(52)).toBe("sa"); // 26 (base) + 26 → row 1, col 0 → "s" + "a"
    for (const label of labelsFor(60).slice(26)) expect(label).toHaveLength(2);
  });

  test("labelFor is deterministic — same n always yields the same label", () => {
    expect(labelFor(5)).toBe(labelFor(5));
    expect(labelFor(100)).toBe(labelFor(100));
  });

  test("labelsFor(n) is exactly labelFor(0..n-1) in order", () => {
    const n = 40;
    expect(labelsFor(n)).toEqual(Array.from({ length: n }, (_, i) => labelFor(i)));
  });

  test("rejects a negative or non-integer index", () => {
    expect(() => labelFor(-1)).toThrow(RangeError);
    expect(() => labelFor(1.5)).toThrow(RangeError);
  });
});
