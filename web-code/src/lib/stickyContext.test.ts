import { describe, expect, it } from "vitest";
import { ancestorChain, stickyChain, type StickySymbol } from "./stickyContext";

function s(
  name: string,
  start: number,
  end: number,
  kind = "function",
): StickySymbol {
  return { name, kind, start_line: start, end_line: end };
}

describe("ancestorChain", () => {
  it("returns empty when no symbols", () => {
    expect(ancestorChain([], 10)).toEqual([]);
  });

  it("returns empty when no symbol contains the line", () => {
    const symbols = [s("a", 1, 5), s("b", 10, 20)];
    expect(ancestorChain(symbols, 7)).toEqual([]);
  });

  it("returns a single containing symbol", () => {
    const symbols = [s("outer", 1, 20)];
    expect(ancestorChain(symbols, 10).map((a) => a.name)).toEqual(["outer"]);
  });

  it("orders nesting outermost → innermost", () => {
    const symbols = [
      s("outer", 1, 40, "class"),
      s("mid", 5, 30, "method"),
      s("inner", 10, 15, "function"),
    ];
    expect(ancestorChain(symbols, 12).map((a) => a.name)).toEqual([
      "outer",
      "mid",
      "inner",
    ]);
  });

  it("includes boundary lines (start and end inclusive)", () => {
    const symbols = [s("f", 5, 10)];
    expect(ancestorChain(symbols, 5).map((a) => a.name)).toEqual(["f"]);
    expect(ancestorChain(symbols, 10).map((a) => a.name)).toEqual(["f"]);
    expect(ancestorChain(symbols, 4)).toEqual([]);
    expect(ancestorChain(symbols, 11)).toEqual([]);
  });

  it("caps at 4 by dropping outermost", () => {
    const symbols = [
      s("a", 1, 100),
      s("b", 2, 90),
      s("c", 3, 80),
      s("d", 4, 70),
      s("e", 5, 60),
    ];
    const chain = ancestorChain(symbols, 10, 4);
    expect(chain.map((a) => a.name)).toEqual(["b", "c", "d", "e"]);
  });

  it("respects a smaller custom cap (breadcrumbs use 3)", () => {
    const symbols = [
      s("a", 1, 100),
      s("b", 2, 90),
      s("c", 3, 80),
      s("d", 4, 70),
    ];
    expect(ancestorChain(symbols, 10, 3).map((x) => x.name)).toEqual([
      "b",
      "c",
      "d",
    ]);
  });

  it("prefers wider span as outer when two ranges share a start", () => {
    // Both start at 1; wider is outer.
    const symbols = [s("inner", 1, 5), s("outer", 1, 20)];
    expect(ancestorChain(symbols, 3).map((a) => a.name)).toEqual([
      "outer",
      "inner",
    ]);
  });
});

describe("stickyChain (sticky stack — scrolled-off ancestors only)", () => {
  const s = (name: string, start_line: number, end_line: number): StickySymbol => ({
    name,
    kind: "fn",
    start_line,
    end_line,
  });

  it("does NOT pin a symbol whose signature IS the first visible line", () => {
    // The 2026-07-31 e2e regression: a fn starting at line 1 pinned while
    // line 1 was visible, overlaying it and stealing its pointer events.
    expect(stickyChain([s("top", 1, 40)], 1)).toEqual([]);
  });

  it("pins once the signature scrolls off (start_line < firstVisibleLine)", () => {
    expect(stickyChain([s("top", 1, 40)], 2).map((a) => a.name)).toEqual(["top"]);
  });

  it("keeps the visible inner symbol out while pinning the scrolled-off outer", () => {
    const symbols = [s("outer", 1, 100), s("inner", 10, 30)];
    // First visible line 10: inner's signature is on screen — only outer pins.
    expect(stickyChain(symbols, 10).map((a) => a.name)).toEqual(["outer"]);
    // First visible line 11: both signatures are off — both pin, outer first.
    expect(stickyChain(symbols, 11).map((a) => a.name)).toEqual(["outer", "inner"]);
  });

  it("returns empty at top of file", () => {
    expect(stickyChain([s("a", 1, 9), s("b", 2, 8)], 1)).toEqual([]);
  });
});
