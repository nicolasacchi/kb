import { describe, expect, it } from "vitest";
import { DIAC, DIAC_INDEX, PH } from "./diacritics";

describe("kitty diacritics table", () => {
  it("has the 297 kitty row/column diacritics", () => {
    expect(DIAC).toHaveLength(297);
    for (let i = 1; i < DIAC.length; i += 1) {
      expect(DIAC[i]).toBeGreaterThan(DIAC[i - 1]);
    }
    expect(DIAC[0]).toBe(773);
    expect(DIAC[296]).toBe(119364);
    expect(DIAC_INDEX.get(773)).toBe(0);
    expect(DIAC_INDEX.get(119364)).toBe(296);
    expect(DIAC_INDEX.get(65)).toBeUndefined();
    expect(DIAC_INDEX.size).toBe(297);
  });

  it("names the placeholder code point", () => {
    expect(PH).toBe(0x10eeee);
  });
});
