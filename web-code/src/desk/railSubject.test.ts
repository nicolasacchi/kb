import { describe, expect, it } from "vitest";
import type { Symbol as CodeSymbol } from "../api/types";
import {
  fileLabel,
  pinnedHeaderText,
  sameSubject,
  subjectFor,
  subjectLabel,
  symbolAtLine,
  symbolLabel,
} from "./railSubject";

function sym(o: Partial<CodeSymbol> & { name: string; line_start: number; line_end: number }): CodeSymbol {
  return {
    ordinal: 0,
    kind: "function",
    col_start: 0,
    col_end: 0,
    container: null,
    signature: null,
    doc: null,
    ...o,
  } as CodeSymbol;
}

const SYMS: CodeSymbol[] = [
  sym({ ordinal: 0, name: "Order", kind: "class", line_start: 1, line_end: 60 }),
  sym({ ordinal: 1, name: "total", kind: "method", line_start: 10, line_end: 20, container: "Order" }),
  sym({ ordinal: 2, name: "refund", kind: "method", line_start: 30, line_end: 40, container: "Order" }),
];

describe("railSubject — labels", () => {
  it("fileLabel is the basename", () => {
    expect(fileLabel("app/models/order.rb")).toBe("order.rb");
    expect(fileLabel("README.md")).toBe("README.md");
  });
  it("symbolLabel qualifies with the container when there is one", () => {
    expect(symbolLabel({ name: "total", container: "Order" })).toBe("Order#total");
    expect(symbolLabel({ name: "main", container: null })).toBe("main");
  });
});

describe("railSubject — symbolAtLine picks the innermost", () => {
  it("prefers the method over the enclosing class", () => {
    expect(symbolAtLine(SYMS, 12)?.name).toBe("total");
    expect(symbolAtLine(SYMS, 35)?.name).toBe("refund");
  });
  it("falls back to the enclosing class between methods", () => {
    expect(symbolAtLine(SYMS, 25)?.name).toBe("Order");
  });
  it("returns null outside every symbol", () => {
    expect(symbolAtLine(SYMS, 100)).toBeNull();
    expect(symbolAtLine([], 5)).toBeNull();
  });
  it("breaks an identical-range tie toward the later ordinal (the nested one)", () => {
    const a = sym({ ordinal: 0, name: "outer", line_start: 1, line_end: 3 });
    const b = sym({ ordinal: 1, name: "inner", line_start: 1, line_end: 3 });
    expect(symbolAtLine([a, b], 2)?.name).toBe("inner");
  });
});

describe("railSubject — the cascade", () => {
  it("no file open is null, not an empty subject", () => {
    expect(subjectFor({ path: null, line: 5, symbols: SYMS, symbolsLoaded: true })).toBeNull();
  });

  it("a caret inside a symbol is a symbol subject with no caption", () => {
    const s = subjectFor({ path: "order.rb", line: 12, symbols: SYMS, symbolsLoaded: true })!;
    expect(s.kind).toBe("symbol");
    expect(s.symbol).toBe("Order#total");
    expect(s.caption).toBeNull();
    expect(subjectLabel(s)).toBe("Order#total");
  });

  it("a caret outside every symbol degrades to line-level WITH a caption", () => {
    const s = subjectFor({ path: "order.rb", line: 100, symbols: SYMS, symbolsLoaded: true })!;
    expect(s.kind).toBe("line");
    expect(s.caption).toBe("caret is outside every symbol — showing line-level");
    expect(subjectLabel(s)).toBe("order.rb:100");
  });

  it("a file with no symbols at all says so", () => {
    const s = subjectFor({ path: "data.json", line: 3, symbols: [], symbolsLoaded: true })!;
    expect(s.kind).toBe("line");
    expect(s.caption).toBe("no symbol data — showing line-level");
  });

  it("no caret yet, no symbols → file-level with the file-level caption", () => {
    const s = subjectFor({ path: "data.json", line: null, symbols: [], symbolsLoaded: true })!;
    expect(s.kind).toBe("file");
    expect(s.caption).toBe("no symbol data — showing file-level");
  });

  it("while the file is still loading, the fallback is UNCAPTIONED — unknown is not empty", () => {
    expect(subjectFor({ path: "x.rb", line: null, symbols: [], symbolsLoaded: false })!.caption).toBeNull();
    expect(subjectFor({ path: "x.rb", line: 4, symbols: [], symbolsLoaded: false })!.caption).toBeNull();
  });

  it("a zero/negative line is treated as no caret", () => {
    expect(subjectFor({ path: "x.rb", line: 0, symbols: SYMS, symbolsLoaded: true })!.kind).toBe("file");
  });
});

describe("railSubject — sameSubject", () => {
  const at = (line: number) => subjectFor({ path: "order.rb", line, symbols: SYMS, symbolsLoaded: true });

  it("a caret moving WITHIN one symbol is the same subject", () => {
    expect(sameSubject(at(11), at(19))).toBe(true);
  });
  it("moving to another symbol is a change", () => {
    expect(sameSubject(at(12), at(35))).toBe(false);
  });
  it("two nulls are the same; one null is not", () => {
    expect(sameSubject(null, null)).toBe(true);
    expect(sameSubject(at(12), null)).toBe(false);
  });
  it("a different file is a change even at the same line", () => {
    const a = subjectFor({ path: "a.rb", line: 3, symbols: [], symbolsLoaded: true });
    const b = subjectFor({ path: "b.rb", line: 3, symbols: [], symbolsLoaded: true });
    expect(sameSubject(a, b)).toBe(false);
  });
  it("a caption change alone is not a subject change", () => {
    const loading = subjectFor({ path: "x.rb", line: 4, symbols: [], symbolsLoaded: false });
    const loaded = subjectFor({ path: "x.rb", line: 4, symbols: [], symbolsLoaded: true });
    expect(loading!.caption).not.toBe(loaded!.caption);
    expect(sameSubject(loading, loaded)).toBe(true);
  });
});

describe("railSubject — the pinned header names BOTH", () => {
  const total = subjectFor({ path: "order.rb", line: 12, symbols: SYMS, symbolsLoaded: true })!;
  const refund = subjectFor({ path: "order.rb", line: 35, symbols: SYMS, symbolsLoaded: true })!;

  it("names only the pinned subject while the caret has not left it", () => {
    expect(pinnedHeaderText(total, total)).toBe("📌 Order#total");
    expect(pinnedHeaderText(total, null)).toBe("📌 Order#total");
  });

  it("names the pinned subject AND where the caret went, with the way out", () => {
    expect(pinnedHeaderText(total, refund)).toBe(
      "📌 Order#total — caret is in Order#refund · unpin to follow",
    );
  });
});
