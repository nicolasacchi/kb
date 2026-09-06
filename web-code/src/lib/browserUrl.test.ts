import { describe, expect, it } from "vitest";
import {
  browserUrl,
  decodeBrowserSymbol,
  encodeBrowserSymbol,
  parseBrowserSearch,
  type BrowserSymbolRef,
} from "./browserUrl";

describe("encodeBrowserSymbol / decodeBrowserSymbol", () => {
  it("round-trips path#name", () => {
    const ref: BrowserSymbolRef = { path: "src/lib.rs", name: "Foo" };
    expect(encodeBrowserSymbol(ref)).toBe("src/lib.rs#Foo");
    expect(decodeBrowserSymbol(encodeBrowserSymbol(ref))).toEqual(ref);
  });

  it("round-trips path#name#line", () => {
    const ref: BrowserSymbolRef = { path: "a/b.rs", name: "draw", line: 12 };
    expect(encodeBrowserSymbol(ref)).toBe("a/b.rs#draw#12");
    expect(decodeBrowserSymbol(encodeBrowserSymbol(ref))).toEqual(ref);
  });

  it("round-trips with container suffix", () => {
    const ref: BrowserSymbolRef = {
      path: "hier_trait.rs",
      name: "draw",
      line: 8,
      container: "Circle",
    };
    const enc = encodeBrowserSymbol(ref);
    expect(enc).toBe("hier_trait.rs#draw#8@Circle");
    expect(decodeBrowserSymbol(enc)).toEqual(ref);
  });

  it("encodes special characters in name", () => {
    const ref: BrowserSymbolRef = { path: "x.rs", name: "foo bar" };
    expect(encodeBrowserSymbol(ref)).toBe("x.rs#foo%20bar");
    expect(decodeBrowserSymbol(encodeBrowserSymbol(ref))).toEqual(ref);
  });

  it("returns null for junk", () => {
    expect(decodeBrowserSymbol(null)).toBeNull();
    expect(decodeBrowserSymbol("")).toBeNull();
    expect(decodeBrowserSymbol("nopath")).toBeNull();
    expect(decodeBrowserSymbol("#onlyname")).toBeNull();
    expect(decodeBrowserSymbol("path#")).toBeNull();
  });

  it("ignores non-finite line", () => {
    expect(decodeBrowserSymbol("a.rs#Foo#nope")).toEqual({ path: "a.rs", name: "Foo" });
  });
});

describe("browserUrl / parseBrowserSearch", () => {
  it("builds bare browser sentinel", () => {
    expect(browserUrl("kb")).toBe("/r/kb/~browser");
    expect(browserUrl("kb", null)).toBe("/r/kb/~browser");
    expect(browserUrl("kb", { symbol: null })).toBe("/r/kb/~browser");
  });

  it("builds with symbol and encodes repo", () => {
    expect(
      browserUrl("my repo", { path: "lib.rs", name: "omniboxTargetFunction", line: 1 }),
    ).toBe("/r/my%20repo/~browser?symbol=lib.rs%23omniboxTargetFunction%231");
  });

  it("parses search params", () => {
    const sp = new URLSearchParams("symbol=lib.rs%23Foo%2310");
    expect(parseBrowserSearch(sp)).toEqual({
      symbol: { path: "lib.rs", name: "Foo", line: 10 },
    });
  });

  it("parses empty search as null symbol", () => {
    expect(parseBrowserSearch(new URLSearchParams())).toEqual({ symbol: null });
  });
});
