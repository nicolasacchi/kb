import { describe, expect, it, vi } from "vitest";
import { findUrls, linkAtCell, linkHint, linkSpansRows, linksAtRow, openExternal, trimUrl, type RowSource } from "./links";

describe("findUrls and trimUrl", () => {
  it("finds http and https, not other schemes or a bare scheme", () => {
    const t = "see https://example.com/a?b=1#c and http://localhost:3000/x, ftp://no https:// nothing";
    expect(findUrls(t).map((m) => m.url)).toEqual(["https://example.com/a?b=1#c", "http://localhost:3000/x"]);
  });

  it("trims trailing sentence punctuation and an unbalanced closing bracket only", () => {
    expect(trimUrl("https://a.dev/x.")).toBe("https://a.dev/x");
    expect(trimUrl("https://a.dev/x,")).toBe("https://a.dev/x");
    expect(trimUrl("https://a.dev/x).")).toBe("https://a.dev/x");
    expect(trimUrl("https://a.dev/x?!")).toBe("https://a.dev/x");
    expect(trimUrl("https://a.dev/w_(b)")).toBe("https://a.dev/w_(b)");
    expect(trimUrl("https://a.dev/w_(b))")).toBe("https://a.dev/w_(b)");
  });

  it("offsets point at the trimmed url", () => {
    const [m] = findUrls("go (https://a.dev/p).");
    expect(m).toEqual({ start: 4, end: 4 + "https://a.dev/p".length, url: "https://a.dev/p" });
  });
});

function src(rows: Array<[string, boolean]>): RowSource {
  return { length: rows.length, row: (y) => (rows[y] ? { text: rows[y][0], wrapped: rows[y][1] } : null) };
}

describe("linksAtRow", () => {
  it("a link inside one row maps to 1-based cells on that row", () => {
    const l = linksAtRow(src([["x https://a.dev/p y", false]]), 0, 40);
    expect(l).toEqual([{ url: "https://a.dev/p", start: { x: 3, y: 1 }, end: { x: 17, y: 1 } }]);
  });

  it("joins wrapped rows: the same link is returned for either row, ending on the second", () => {
    const rows: Array<[string, boolean]> = [
      ["go https://exam", false],
      ["ple.com/path now", true],
    ];
    const a = linksAtRow(src(rows), 0, 15);
    const b = linksAtRow(src(rows), 1, 15);
    expect(a).toHaveLength(1);
    expect(b).toEqual(a);
    expect(a[0].url).toBe("https://example.com/path");
    expect(a[0].start).toEqual({ x: 4, y: 1 });
    expect(a[0].end).toEqual({ x: 12, y: 2 });
  });

  it("does not join across a row that is not a continuation", () => {
    const l = linksAtRow(src([["https://a.dev/ab", false], ["cd", false]]), 1, 16);
    expect(l).toEqual([]);
  });

  it("returns only links that touch the asked row", () => {
    const rows: Array<[string, boolean]> = [
      ["https://a.dev/1 aaaa", false],
      ["bbbbbbbbbbbbbbbbbbbb", false],
      ["https://b.dev/2", false],
    ];
    expect(linksAtRow(src(rows), 2, 20).map((x) => x.url)).toEqual(["https://b.dev/2"]);
    expect(linksAtRow(src(rows), 1, 20)).toEqual([]);
    expect(linksAtRow(src(rows), 9, 20)).toEqual([]);
  });
});

describe("openExternal", () => {
  it("opens http(s) in a new tab without opener or referrer, and refuses anything else", () => {
    const open = vi.fn();
    openExternal("https://a.dev/x", { open });
    expect(open).toHaveBeenCalledWith("https://a.dev/x", "_blank", "noopener,noreferrer");
    openExternal("javascript:alert(1)", { open });
    openExternal("file:///etc/passwd", { open });
    expect(open).toHaveBeenCalledTimes(1);
  });
});

describe("FUPHONELINKS linkAtCell", () => {
  const cols = 20;
  it("hits only a cell inside the url; the cells around it miss", () => {
    const s = src([["go https://a.dev/x ok", false]]);
    // "https://a.dev/x" starts at offset 3 (cell 4) and is 15 cells long (cells 4..18).
    expect(linkAtCell(s, 40, 4, 0)?.url).toBe("https://a.dev/x");
    expect(linkAtCell(s, 40, 18, 0)?.url).toBe("https://a.dev/x");
    expect(linkAtCell(s, 40, 3, 0)).toBeNull();
    expect(linkAtCell(s, 40, 19, 0)).toBeNull();
    expect(linkAtCell(s, 40, 1, 5)).toBeNull();
  });

  it("a url wrapped across rows resolves whole from a cell on either row", () => {
    const s = src([
      ["see https://example.", false],
      ["com/path done", true],
    ]);
    // Row 0 is a full 20-cell row; the url runs from cell 5 of row 0 to cell 8 of row 1.
    const a = linkAtCell(s, cols, 6, 0);
    const b = linkAtCell(s, cols, 3, 1);
    expect(a?.url).toBe("https://example.com/path");
    expect(b?.url).toBe("https://example.com/path");
    expect(a && linkSpansRows(a)).toBe(true);
    expect(linkAtCell(s, cols, 4, 0)).toBeNull();
    expect(linkAtCell(s, cols, 9, 1)).toBeNull();
  });

  it("a single-row link does not span rows", () => {
    const l = linkAtCell(src([["https://a.dev/x", false]]), 40, 2, 0)!;
    expect(linkSpansRows(l)).toBe(false);
  });
});

describe("FUPHONELINKS linkHint", () => {
  it("names the modifier of the platform", () => {
    expect(linkHint(false)).toBe("Ctrl+click to open");
    expect(linkHint(true)).toBe("Cmd+click to open");
  });
});
