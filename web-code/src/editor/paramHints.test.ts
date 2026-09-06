import { describe, expect, it } from "vitest";
import {
  buildHintsForSites,
  findCallSitesInRange,
  isLiteralIshArg,
  parseSignatureParamNames,
  shouldSkipArgHint,
  type CallSiteHit,
} from "./paramHints";

describe("parseSignatureParamNames", () => {
  it("parses Rust fn signatures", () => {
    expect(parseSignatureParamNames("fn f(a: T, b: U)")).toEqual(["a", "b"]);
    expect(parseSignatureParamNames("pub fn paint(mut self, color: Color)")).toEqual(["color"]);
    expect(parseSignatureParamNames("fn helper() -> i32")).toEqual([]);
  });

  it("parses Python def signatures", () => {
    expect(parseSignatureParamNames("def f(a, b=1)")).toEqual(["a", "b"]);
    expect(parseSignatureParamNames("def method(self, x: int, y=0)")).toEqual(["x", "y"]);
  });

  it("parses JS/TS function signatures", () => {
    expect(parseSignatureParamNames("function f(a, b)")).toEqual(["a", "b"]);
    expect(parseSignatureParamNames("function g(a: number, b: string): void")).toEqual(["a", "b"]);
  });

  it("returns null when unparseable", () => {
    expect(parseSignatureParamNames(null)).toBeNull();
    expect(parseSignatureParamNames("")).toBeNull();
    expect(parseSignatureParamNames("just a comment")).toBeNull();
  });
});

describe("isLiteralIshArg / shouldSkipArgHint", () => {
  it("accepts numbers, strings, bools, null/None", () => {
    expect(isLiteralIshArg("42")).toBe(true);
    expect(isLiteralIshArg("-3.5")).toBe(true);
    expect(isLiteralIshArg('"hi"')).toBe(true);
    expect(isLiteralIshArg("'x'")).toBe(true);
    expect(isLiteralIshArg("true")).toBe(true);
    expect(isLiteralIshArg("None")).toBe(true);
    expect(isLiteralIshArg("null")).toBe(true);
    expect(isLiteralIshArg("foo")).toBe(false);
    expect(isLiteralIshArg("foo.bar")).toBe(false);
  });

  it("skips when arg ident equals param name", () => {
    expect(shouldSkipArgHint("color", "color")).toBe(true);
    expect(shouldSkipArgHint("Color", "color")).toBe(true);
    expect(shouldSkipArgHint("42", "color")).toBe(false);
  });
});

describe("findCallSitesInRange + buildHintsForSites", () => {
  it("finds call sites and builds name widgets for literals", () => {
    const doc = "fn main() {\n    paint(1, 2);\n}\n";
    const lineAt = (pos: number) => {
      const lines = doc.split("\n");
      let off = 0;
      for (let i = 0; i < lines.length; i++) {
        const len = lines[i].length + (i < lines.length - 1 ? 1 : 0);
        if (pos < off + len || i === lines.length - 1) {
          return { number: i + 1, from: off };
        }
        off += len;
      }
      return { number: 1, from: 0 };
    };
    const sites = findCallSitesInRange(doc, 0, doc.length, lineAt);
    const paint = sites.find((s) => s.calleeName === "paint");
    expect(paint).toBeDefined();
    expect(paint!.args.length).toBe(2);

    const hints = buildHintsForSites([paint as CallSiteHit], (n) =>
      n === "paint" ? ["a", "b"] : null,
    );
    expect(hints.map((h) => h.name)).toEqual(["a", "b"]);
  });
});
