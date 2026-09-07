import { describe, expect, it } from "vitest";
import { parseInline, parseMarkdownLite } from "./markdownLite";

describe("parseInline", () => {
  it("returns one plain text run for text with no markers", () => {
    expect(parseInline("plain text")).toEqual([{ kind: "text", text: "plain text" }]);
  });

  it("returns an empty run list for an empty string", () => {
    expect(parseInline("")).toEqual([]);
  });

  it("parses a bold span", () => {
    expect(parseInline("**Trade::TableComponent** now accepts")).toEqual([
      { kind: "bold", text: "Trade::TableComponent" },
      { kind: "text", text: " now accepts" },
    ]);
  });

  it("parses an inline code span", () => {
    expect(parseInline("the `search_url:` kwarg")).toEqual([
      { kind: "text", text: "the " },
      { kind: "code", text: "search_url:" },
      { kind: "text", text: " kwarg" },
    ]);
  });

  it("parses bold and code together in one line", () => {
    expect(parseInline("**Tests**: new `table_component_spec.rb` block")).toEqual([
      { kind: "bold", text: "Tests" },
      { kind: "text", text: ": new " },
      { kind: "code", text: "table_component_spec.rb" },
      { kind: "text", text: " block" },
    ]);
  });

  it("degrades an unmatched ** to plain text rather than eating the rest of the line", () => {
    expect(parseInline("2 ** 8 is 256")).toEqual([{ kind: "text", text: "2 ** 8 is 256" }]);
  });

  it("degrades an unmatched backtick to plain text", () => {
    expect(parseInline("a lone ` backtick")).toEqual([{ kind: "text", text: "a lone ` backtick" }]);
  });

  it("treats an empty bold/code span as plain text (non-empty span required)", () => {
    expect(parseInline("empty **** span")).toEqual([{ kind: "text", text: "empty **** span" }]);
    expect(parseInline("empty `` span")).toEqual([{ kind: "text", text: "empty `` span" }]);
  });
});

describe("parseMarkdownLite", () => {
  it("returns no blocks for an empty or whitespace-only source", () => {
    expect(parseMarkdownLite("")).toEqual([]);
    expect(parseMarkdownLite("   \n  \n")).toEqual([]);
  });

  it("parses a single paragraph, joining wrapped lines with a space", () => {
    const blocks = parseMarkdownLite("first line\nsecond line");
    expect(blocks).toEqual([
      { kind: "paragraph", runs: [{ kind: "text", text: "first line second line" }] },
    ]);
  });

  it("splits two paragraphs on a blank line", () => {
    const blocks = parseMarkdownLite("para one\n\npara two");
    expect(blocks.map((b) => b.kind)).toEqual(["paragraph", "paragraph"]);
  });

  it("groups consecutive bullet lines into one list block", () => {
    const blocks = parseMarkdownLite("- item one\n- item two\n* item three");
    expect(blocks).toHaveLength(1);
    expect(blocks[0]).toMatchObject({ kind: "list" });
    if (blocks[0].kind === "list") {
      expect(blocks[0].items).toHaveLength(3);
      expect(blocks[0].items[0]).toEqual([{ kind: "text", text: "item one" }]);
    }
  });

  it("parses a paragraph, then a list, matching the report-pr.html summary shape", () => {
    const src =
      "Partner QA reported the tab's search boxes go dead.\n\n" +
      "- **Trade::TableComponent** now accepts `search_url:`.\n" +
      "- **PaginatorComponent** gains `base_url:`.";
    const blocks = parseMarkdownLite(src);
    expect(blocks.map((b) => b.kind)).toEqual(["paragraph", "list"]);
    expect(blocks[1]).toMatchObject({ kind: "list" });
    if (blocks[1].kind === "list") {
      expect(blocks[1].items).toHaveLength(2);
    }
  });

  it("a bullet line immediately after a paragraph line (no blank) still starts a new list block", () => {
    const blocks = parseMarkdownLite("some prose\n- a bullet");
    expect(blocks.map((b) => b.kind)).toEqual(["paragraph", "list"]);
  });
});

// --- V73-K2b — the two OPT-IN block kinds ---------------------------------
//
// Both are off by default, and the first case here is the one that matters:
// a caller that does not pass options gets PRR-U2's stream byte-for-byte, so
// `ReportPanel`'s rendered summary is unchanged by their existence.
describe("markdownLite options", () => {
  it("leaves a heading line as prose when headings are OFF", () => {
    expect(parseMarkdownLite("# Title\n\ntext")).toEqual([
      { kind: "paragraph", runs: [{ kind: "text", text: "# Title" }] },
      { kind: "paragraph", runs: [{ kind: "text", text: "text" }] },
    ]);
  });

  it("leaves a fence as prose when fences are OFF", () => {
    const blocks = parseMarkdownLite("```rb\nx = 1\n```");
    expect(blocks.every((b) => b.kind === "paragraph")).toBe(true);
  });

  it("parses six heading levels when headings are ON", () => {
    const blocks = parseMarkdownLite("# a\n## b\n###### c", { headings: true });
    expect(blocks.map((b) => (b.kind === "heading" ? b.level : null))).toEqual([1, 2, 6]);
  });

  it("does not treat a seventh hash as a heading", () => {
    const blocks = parseMarkdownLite("####### too deep", { headings: true });
    expect(blocks[0].kind).toBe("paragraph");
  });

  it("keeps a fence's body verbatim, with its info string", () => {
    const blocks = parseMarkdownLite("```rb\n  x = 1\n\n  y\n```", { fences: true });
    expect(blocks).toEqual([{ kind: "code", lang: "rb", text: "  x = 1\n\n  y" }]);
  });

  it("closes a fence only on a run at least as long as the opener", () => {
    const blocks = parseMarkdownLite("````\n```\nstill inside\n````", { fences: true });
    expect(blocks).toEqual([{ kind: "code", lang: null, text: "```\nstill inside" }]);
  });

  it("emits an UNCLOSED fence with what it holds rather than swallowing the tail", () => {
    const blocks = parseMarkdownLite("```\nx = 1\n", { fences: true });
    expect(blocks).toEqual([{ kind: "code", lang: null, text: "x = 1\n" }]);
  });

  it("does not parse markdown inside a fence", () => {
    const blocks = parseMarkdownLite("```\n**not bold** and `not code`\n```", { fences: true });
    expect(blocks).toEqual([
      { kind: "code", lang: null, text: "**not bold** and `not code`" },
    ]);
  });
});
