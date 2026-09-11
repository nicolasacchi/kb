import { describe, expect, it } from "vitest";
import {
  suggestionCaption,
  suggestionDiff,
  suggestionRenderRows,
  tokenize,
  tokenDiff,
  TOKEN_DIFF_LINE_FALLBACK,
} from "./tokenDiff";

describe("tokenize", () => {
  it("splits identifiers, punctuation, and whitespace runs", () => {
    expect(tokenize("foo = bar")).toEqual([
      { text: "foo", kind: "ident" },
      { text: " ", kind: "ws" },
      { text: "=", kind: "punct" },
      { text: " ", kind: "ws" },
      { text: "bar", kind: "ident" },
    ]);
  });

  it("keeps a whitespace run as one token", () => {
    expect(tokenize("a  \tb")).toEqual([
      { text: "a", kind: "ident" },
      { text: "  \t", kind: "ws" },
      { text: "b", kind: "ident" },
    ]);
  });
});

describe("tokenDiff goldens", () => {
  it("insert", () => {
    const ops = tokenDiff("foo", "foo bar");
    expect(ops.map((o) => [o.kind, o.text])).toEqual([
      ["eq", "foo"],
      ["add", " "],
      ["add", "bar"],
    ]);
    expect(ops.filter((o) => o.kind !== "eq").every((o) => o.trailing)).toBe(true);
  });

  it("delete", () => {
    const ops = tokenDiff("foo bar", "foo");
    expect(ops.map((o) => [o.kind, o.text])).toEqual([
      ["eq", "foo"],
      ["del", " "],
      ["del", "bar"],
    ]);
  });

  it("replace", () => {
    const ops = tokenDiff("return a;", "return b;");
    expect(ops.map((o) => [o.kind, o.text])).toEqual([
      ["eq", "return"],
      ["eq", " "],
      ["del", "a"],
      ["add", "b"],
      ["eq", ";"],
    ]);
  });

  it("whitespace-only", () => {
    const ops = tokenDiff("a b", "a  b");
    expect(ops.map((o) => [o.kind, o.text, o.tokenKind])).toEqual([
      ["eq", "a", "ident"],
      ["del", " ", "ws"],
      ["add", "  ", "ws"],
      ["eq", "b", "ident"],
    ]);
  });

  it("identical", () => {
    const ops = tokenDiff("same line", "same line");
    expect(ops.every((o) => o.kind === "eq")).toBe(true);
    expect(ops.map((o) => o.text).join("")).toBe("same line");
  });

  it("marks a #test-style trailing edit", () => {
    const ops = tokenDiff("value", "value #test");
    const changed = ops.filter((o) => o.kind !== "eq");
    expect(changed.map((o) => o.text).join("")).toBe(" #test");
    expect(changed.every((o) => o.trailing)).toBe(true);
  });
});

describe("suggestionCaption", () => {
  it("token mode names lines, tokens, and ±", () => {
    expect(
      suggestionCaption({
        mode: "token",
        linesChanged: 2,
        tokensChanged: 5,
        added: 1,
        deleted: 1,
        totalLines: 4,
      }),
    ).toBe("changes 2 lines · 5 tokens · +1 −1");
  });

  it("identical is a word, not a zero-change formula", () => {
    expect(
      suggestionCaption({
        mode: "token",
        linesChanged: 0,
        tokensChanged: 0,
        added: 0,
        deleted: 0,
        totalLines: 3,
      }),
    ).toBe("identical");
  });

  it("line-level fallback names the threshold", () => {
    expect(
      suggestionCaption({
        mode: "line",
        linesChanged: 10,
        tokensChanged: 0,
        added: 6,
        deleted: 4,
        totalLines: TOKEN_DIFF_LINE_FALLBACK,
      }),
    ).toBe("changes 10 lines · line-level (suggestion is 200+ lines) · +6 −4");
  });
});

describe("suggestionDiff", () => {
  it("dims identical lines and emphasises changed tokens", () => {
    const view = suggestionDiff("keep\nfoo = 1\n", "keep\nfoo = 2\n");
    expect(view.mode).toBe("token");
    expect(view.lines).toHaveLength(2);
    expect(view.lines[0].kind).toBe("eq");
    expect(view.lines[1].kind).toBe("replace");
    expect(view.lines[1].ops.filter((o) => o.kind !== "eq").map((o) => o.text)).toEqual(["1", "2"]);
    expect(view.caption).toBe("changes 1 lines · 2 tokens · +1 −1");
  });

  it("identical bodies caption as identical", () => {
    const view = suggestionDiff("a\nb", "a\nb");
    expect(view.linesChanged).toBe(0);
    expect(view.tokensChanged).toBe(0);
    expect(view.caption).toBe("identical");
  });

  it("a rendered NEW row's text equals the replacement line exactly", () => {
    const original = "e2e-suggest-target-line";
    const replacement = "e2e-suggest-replacement-a";
    const view = suggestionDiff(original, replacement);
    const rows = view.lines.flatMap(suggestionRenderRows);
    const oldRow = rows.find((r) => r.side === "old");
    const newRow = rows.find((r) => r.side === "new");
    expect(oldRow?.text).toBe(original);
    expect(newRow?.text).toBe(replacement);
    expect(oldRow?.ops.map((o) => o.text).join("")).toBe(original);
    expect(newRow?.ops.map((o) => o.text).join("")).toBe(replacement);
  });

  it("a trailing suffix still paints two verbatim rows", () => {
    const original = "e2e-r2c-target-line";
    const replacement = "e2e-r2c-target-line #changed";
    const view = suggestionDiff(original, replacement);
    const rows = view.lines.flatMap(suggestionRenderRows);
    expect(rows.find((r) => r.side === "old")?.text).toBe(original);
    expect(rows.find((r) => r.side === "new")?.text).toBe(replacement);
    expect(view.caption).toContain("tokens");
  });

  it("large suggestions fall back to line-level", () => {
    const oldLines = Array.from({ length: TOKEN_DIFF_LINE_FALLBACK + 1 }, (_, i) => `old ${i}`);
    const newLines = oldLines.map((l, i) => (i === 0 ? "new 0" : l));
    const view = suggestionDiff(oldLines.join("\n"), newLines.join("\n"));
    expect(view.mode).toBe("line");
    expect(view.caption).toContain("line-level");
    expect(view.caption).toContain("200+ lines");
    expect(view.lines[0].kind).toBe("del");
    expect(view.lines[1].kind).toBe("add");
    expect(view.lines[0].ops).toHaveLength(1);
  });
});
