import { describe, expect, it } from "vitest";
import { decodePlaceholder } from "./decode";
import { DIAC, PH } from "./diacritics";

const ph = String.fromCodePoint(PH);
const d = (i: number): string => String.fromCodePoint(DIAC[i]);

describe("decodePlaceholder", () => {
  it("rgb id", () => {
    const cell = decodePlaceholder(ph + d(2) + d(5), true, false, 0x010203, null);
    expect(cell).toEqual({ id: 0x010203, row: 2, col: 5 });
  });

  it("palette id", () => {
    // Only the low 8 bits count for a palette foreground.
    const cell = decodePlaceholder(ph + d(0) + d(0), false, true, 0x1234d, null);
    expect(cell).toEqual({ id: 0x4d, row: 0, col: 0 });
    expect(decodePlaceholder(ph + d(0) + d(0), false, false, 0xffffff, null)?.id).toBe(0);
  });

  it("explicit row and col", () => {
    const prev = { id: 7, row: 9, col: 9 };
    // Explicit marks win over the previous cell.
    expect(decodePlaceholder(ph + d(3) + d(4), true, false, 7, prev)).toEqual({
      id: 7,
      row: 3,
      col: 4,
    });
  });

  it("fourth codepoint is id high byte", () => {
    const cell = decodePlaceholder(ph + d(1) + d(1) + d(1), true, false, 0x0a0b0c, null);
    expect(cell?.id).toBe(0x010a0b0c);
    // High byte >= 128 stays an unsigned 32-bit id.
    const big = decodePlaceholder(ph + d(0) + d(0) + d(200), true, false, 5, null);
    expect(big?.id).toBe(((200 << 24) | 5) >>> 0);
    expect(big?.id).toBeGreaterThan(0);
  });

  it("no diacritics continues previous cell", () => {
    const prev = { id: 77, row: 2, col: 6 };
    expect(decodePlaceholder(ph, true, false, 77, prev)).toEqual({ id: 77, row: 2, col: 7 });
  });

  it("row without col continues same row", () => {
    const prev = { id: 77, row: 4, col: 3 };
    expect(decodePlaceholder(ph + d(4), true, false, 77, prev)).toEqual({
      id: 77,
      row: 4,
      col: 4,
    });
    // A different row starts a new run at column 0.
    expect(decodePlaceholder(ph + d(5), true, false, 77, prev)).toEqual({
      id: 77,
      row: 5,
      col: 0,
    });
    expect(decodePlaceholder(ph + d(4), true, false, 77, null)).toEqual({
      id: 77,
      row: 4,
      col: 0,
    });
  });

  it("different id resets", () => {
    const prev = { id: 77, row: 2, col: 6 };
    expect(decodePlaceholder(ph, true, false, 78, prev)).toEqual({ id: 78, row: 0, col: 0 });
    expect(decodePlaceholder(ph + d(2), true, false, 78, prev)).toEqual({
      id: 78,
      row: 2,
      col: 0,
    });
  });

  it("non-placeholder returns null", () => {
    expect(decodePlaceholder("A", true, false, 1, null)).toBeNull();
    expect(decodePlaceholder("", true, false, 1, null)).toBeNull();
    expect(decodePlaceholder(d(0) + ph, true, false, 1, null)).toBeNull();
  });

  it("lone PH without diacritics", () => {
    expect(decodePlaceholder(ph, true, false, 9, null)).toEqual({ id: 9, row: 0, col: 0 });
  });
});
