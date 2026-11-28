import { describe, expect, it } from "vitest";
import { displayName } from "./displayName";

describe("displayName", () => {
  it("keeps ordinary Unicode and punctuation untouched", () => {
    for (const n of ["résumé final.pdf", "日本語.txt", "a b'c\"d$(x).txt", "emoji-😀.png", "<b>x</b>.html"]) {
      expect(displayName(n)).toBe(n);
    }
  });

  it("escapes C0, DEL and C1 controls", () => {
    expect(displayName("a\nb\tc\u0000d\u007fe\u0085f")).toBe("a\\u{a}b\\u{9}c\\u{0}d\\u{7f}e\\u{85}f");
  });

  it("escapes bidi overrides, isolates and zero-width characters", () => {
    expect(displayName("x\u202ely.exe")).toBe("x\\u{202e}ly.exe");
    expect(displayName("a\u2066b\u2069")).toBe("a\\u{2066}b\\u{2069}");
    expect(displayName("a\u200fb\u200bc\ufeff")).toBe("a\\u{200f}b\\u{200b}c\\u{feff}");
  });
});
