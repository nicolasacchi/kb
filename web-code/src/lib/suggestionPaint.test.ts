import { describe, expect, it } from "vitest";
import type { HighlightOut, HighlightSpan } from "../api/types";
import { paintSpans } from "./paintSpans";
import {
  composePaintedRows,
  composeTokenAndSyntax,
  suggestionHighlightItems,
  type ComposedSeg,
} from "./suggestionPaint";
import { suggestionDiff, suggestionRenderRows, type TokenOp } from "./tokenDiff";

function hl(spans: HighlightSpan[], tier = "tree-sitter"): HighlightOut {
  return {
    schema: "highlight/1",
    lang: "ruby",
    tier,
    spans,
    honesty: { tier, engine: "tree-sitter", derived_from: "path" },
  };
}

function noneResult(reason = "no grammar"): HighlightOut {
  return {
    schema: "highlight/1",
    lang: null,
    tier: "none",
    spans: [],
    honesty: { tier: "none", engine: "none", derived_from: "path", reason },
  };
}

function classNames(seg: ComposedSeg): string[] {
  const out = [`kbc-sugdiff__tok--${seg.tokKind}`];
  if (seg.hlCls) out.push(seg.hlCls);
  return out;
}

describe("suggestionHighlightItems", () => {
  it("emits old+new with lang null so the server derives language from path", () => {
    expect(suggestionHighlightItems("old line", "new line", "src/app.rb")).toEqual([
      { id: "old", lang: null, text: "old line", path: "src/app.rb" },
      { id: "new", lang: null, text: "new line", path: "src/app.rb" },
    ]);
  });

  it("omits an empty side so the batch is one or two items, never a blank payload", () => {
    expect(suggestionHighlightItems("", "only new", "a.rs")).toEqual([
      { id: "new", lang: null, text: "only new", path: "a.rs" },
    ]);
  });
});

describe("composeTokenAndSyntax", () => {
  it("a keyword that is also an added token carries both the syntax class and the add mark", () => {
    const original = "foo()";
    const replacement = "return foo()";
    const view = suggestionDiff(original, replacement);
    const newRow = view.lines.flatMap(suggestionRenderRows).find((r) => r.side === "new");
    expect(newRow).toBeDefined();
    const painted = paintSpans(replacement, [{ line: 1, start: 0, end: 6, role: "keyword" }])[0];
    const segs = composeTokenAndSyntax(newRow!.ops, painted);
    const ret = segs.find((s) => s.text === "return");
    expect(ret).toBeDefined();
    expect(ret!.tokKind).toBe("add");
    expect(ret!.hlCls).toBe("kbc-hl-keyword");
    expect(classNames(ret!)).toEqual(["kbc-sugdiff__tok--add", "kbc-hl-keyword"]);
  });

  it("splits a token when syntax covers only part of it (intersection, not nesting)", () => {
    const ops: TokenOp[] = [{ kind: "add", text: "return", tokenKind: "ident" }];
    const painted = paintSpans("return", [{ line: 1, start: 0, end: 3, role: "keyword" }])[0];
    const segs = composeTokenAndSyntax(ops, painted);
    expect(segs).toEqual([
      { text: "ret", tokKind: "add", hlCls: "kbc-hl-keyword" },
      { text: "urn", tokKind: "add" },
    ]);
    expect(segs.map((s) => s.text).join("")).toBe("return");
  });

  it("in-flight (no paint) keeps add/del marks and does not invent syntax classes", () => {
    const original = "foo()";
    const replacement = "return foo()";
    const newRow = suggestionDiff(original, replacement)
      .lines.flatMap(suggestionRenderRows)
      .find((r) => r.side === "new")!;
    const segs = composeTokenAndSyntax(newRow.ops, undefined);
    expect(segs.every((s) => s.hlCls === undefined)).toBe(true);
    expect(segs.some((s) => s.tokKind === "add")).toBe(true);
    expect(segs.map((s) => s.text).join("")).toBe(replacement);
  });

  it("refused / unclassed paint is unpainted but still diffed", () => {
    const replacement = "return foo()";
    const newRow = suggestionDiff("foo()", replacement)
      .lines.flatMap(suggestionRenderRows)
      .find((r) => r.side === "new")!;
    const segs = composeTokenAndSyntax(newRow.ops, [{ text: replacement }]);
    expect(segs.every((s) => s.hlCls === undefined)).toBe(true);
    expect(segs.some((s) => s.tokKind === "add")).toBe(true);
    expect(segs.map((s) => s.text).join("")).toBe(replacement);
  });

  it("mismatched paint (integrity) is ignored; the verbatim line still concatenates", () => {
    const ops: TokenOp[] = [{ kind: "add", text: "return foo()", tokenKind: "ident" }];
    const segs = composeTokenAndSyntax(ops, [{ text: "NOT THE LINE", cls: "kbc-hl-keyword" }]);
    expect(segs.every((s) => s.hlCls === undefined)).toBe(true);
    expect(segs.map((s) => s.text).join("")).toBe("return foo()");
  });
});

describe("composePaintedRows", () => {
  it("NEW-row segment texts concatenate to the verbatim replacement line after painting", () => {
    const original = "e2e-suggest-target-line";
    const replacement = "e2e-suggest-replacement-a";
    const { rows } = composePaintedRows(
      original,
      replacement,
      hl([{ line: 1, start: 0, end: 3, role: "function" }]),
      hl([{ line: 1, start: 0, end: 3, role: "keyword" }]),
    );
    const oldRow = rows.find((r) => r.side === "old");
    const newRow = rows.find((r) => r.side === "new");
    expect(oldRow?.segs.map((s) => s.text).join("")).toBe(original);
    expect(newRow?.segs.map((s) => s.text).join("")).toBe(replacement);
    expect(newRow?.text).toBe(replacement);
    expect(newRow?.segs.some((s) => s.hlCls === "kbc-hl-keyword")).toBe(true);
  });

  it("in-flight rows are pending, unpainted, and still two-row token-diffed", () => {
    const original = "e2e-r2c-target-line";
    const replacement = "e2e-r2c-target-line #changed";
    const { rows, caption } = composePaintedRows(original, replacement, undefined, undefined);
    expect(caption).toContain("tokens");
    expect(rows.map((r) => r.side)).toEqual(["old", "new"]);
    expect(rows.every((r) => r.tier === "pending")).toBe(true);
    expect(rows.every((r) => r.segs.every((s) => s.hlCls === undefined))).toBe(true);
    expect(rows.find((r) => r.side === "old")?.segs.map((s) => s.text).join("")).toBe(original);
    expect(rows.find((r) => r.side === "new")?.segs.map((s) => s.text).join("")).toBe(replacement);
    expect(rows.find((r) => r.side === "new")?.segs.some((s) => s.tokKind === "add")).toBe(true);
  });

  it("tier none refuses paint and keeps the token diff", () => {
    const original = "foo()";
    const replacement = "return foo()";
    const { rows } = composePaintedRows(original, replacement, noneResult(), noneResult());
    expect(rows.every((r) => r.tier === "none")).toBe(true);
    expect(rows.every((r) => r.segs.every((s) => s.hlCls === undefined))).toBe(true);
    expect(rows.find((r) => r.side === "new")?.segs.some((s) => s.tokKind === "add")).toBe(true);
    expect(rows.find((r) => r.side === "new")?.segs.map((s) => s.text).join("")).toBe(replacement);
  });

  it("an unchanged keyword keeps syntax colour without an add mark", () => {
    const { rows } = composePaintedRows(
      "return a",
      "return b",
      hl([{ line: 1, start: 0, end: 6, role: "keyword" }]),
      hl([{ line: 1, start: 0, end: 6, role: "keyword" }]),
    );
    const newRow = rows.find((r) => r.side === "new")!;
    const ret = newRow.segs.find((s) => s.text === "return")!;
    expect(ret.tokKind).toBe("eq");
    expect(ret.hlCls).toBe("kbc-hl-keyword");
    const added = newRow.segs.find((s) => s.text === "b")!;
    expect(added.tokKind).toBe("add");
  });
});
