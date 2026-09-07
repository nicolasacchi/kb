// V72-I2 — the annotaterb banner parser.
//
// The rule under test is narrowness: this parser reads the banner the gem
// writes and REFUSES everything else, because the card built on it is
// captioned as a client-side read of a comment. A guess here would be a
// fact on screen the daemon never vouched for.
import { describe, expect, it } from "vitest";
import { foldLabel, foldRangeIsValid, isFoldable, parseSchemaBlock } from "./annotaterb";

const BANNER = [
  "# == Schema Information",
  "#",
  "# Table name: orders",
  "#",
  "#  id         :bigint           not null, primary key",
  "#  state      :string           default(\"new\"), not null",
  "#  total_cents :integer",
  "#  created_at :datetime         not null",
  "#",
  "# Indexes",
  "#",
  "#  index_orders_on_state  (state)",
  "#",
  "class Order < ApplicationRecord",
  "end",
  "",
].join("\n");

describe("parseSchemaBlock", () => {
  it("returns null for a file with no banner — never an empty table", () => {
    expect(parseSchemaBlock("class Order < ApplicationRecord\nend\n")).toBeNull();
    expect(parseSchemaBlock("")).toBeNull();
    // A comment that merely mentions the words is not the banner.
    expect(parseSchemaBlock("# see the Schema Information docs\n")).toBeNull();
  });

  it("reads the table name, every column and its own line range", () => {
    const b = parseSchemaBlock(BANNER)!;
    expect(b.tableName).toBe("orders");
    expect(b.startLine).toBe(1);
    // The block is ONE contiguous comment run: it ends on the last `#` line
    // (13), never on the `class` line below it.
    expect(b.endLine).toBe(13);
    expect(b.columns.map((c) => c.name)).toEqual([
      "id",
      "state",
      "total_cents",
      "created_at",
    ]);
    expect(b.columns.map((c) => c.type)).toEqual(["bigint", "string", "integer", "datetime"]);
    expect(b.columns[1].modifiers).toBe('default("new"), not null');
    // Absent modifiers are an empty string, not a guess.
    expect(b.columns[2].modifiers).toBe("");
    // Line numbers are 1-based and address the real file.
    expect(BANNER.split("\n")[b.columns[0].line - 1]).toContain(":bigint");
  });

  it("captures a trailing section verbatim rather than parsing it", () => {
    const b = parseSchemaBlock(BANNER)!;
    expect(b.sections.map((s) => s.title)).toEqual(["Indexes"]);
    // `#` + ONE space is stripped; any further indentation survives, because
    // a section body is rendered in a `<pre>` and its shape is part of it.
    expect(b.sections[0].lines).toEqual([" index_orders_on_state  (state)"]);
    // An index row is NOT mistaken for a column.
    expect(b.columns.some((c) => c.name.startsWith("index_"))).toBe(false);
  });

  it("stops at the trailer when the gem is configured to write one", () => {
    const src = [
      "# == Schema Information",
      "#",
      "# Table name: widgets",
      "#",
      "#  id :bigint",
      "# == Schema Information Trailer",
      "#",
      "# an unrelated comment that is NOT part of the banner",
      "class Widget; end",
    ].join("\n");
    const b = parseSchemaBlock(src)!;
    expect(b.endLine).toBe(6);
    expect(b.columns).toHaveLength(1);
    expect(b.unparsed).toEqual([]);
  });

  it("keeps an unrecognised banner line rather than dropping it", () => {
    const src = [
      "# == Schema Information",
      "#",
      "# Table name: things",
      "# something this parser has never seen",
      "#  id :bigint",
      "",
    ].join("\n");
    const b = parseSchemaBlock(src)!;
    expect(b.unparsed).toEqual(["something this parser has never seen"]);
    expect(b.columns.map((c) => c.name)).toEqual(["id"]);
  });

  it("accepts the older `annotate` spelling", () => {
    const b = parseSchemaBlock("# == Schema Info\n#\n# Table name: t\n#  id :bigint\n")!;
    expect(b.tableName).toBe("t");
  });

  it("does not run past a blank line into a second comment block", () => {
    const src = ["# == Schema Information", "#  id :bigint", "", "# a later comment", "#  fake :string"].join("\n");
    const b = parseSchemaBlock(src)!;
    expect(b.endLine).toBe(2);
    expect(b.columns.map((c) => c.name)).toEqual(["id"]);
  });
});

describe("the fold", () => {
  it("only folds a block worth folding", () => {
    expect(isFoldable(null)).toBe(false);
    expect(isFoldable(parseSchemaBlock("# == Schema Information\n#  id :bigint\n"))).toBe(false);
    expect(isFoldable(parseSchemaBlock(BANNER))).toBe(true);
  });

  it("labels the placeholder with the daemon-free facts it has", () => {
    expect(foldLabel(parseSchemaBlock(BANNER)!)).toBe(
      "== Schema Information · orders · 4 columns · 13 lines",
    );
    // Singular, and an honest "schema" when the banner named no table.
    const b = parseSchemaBlock("# == Schema Information\n#  id :bigint\n#\n#\n")!;
    expect(foldLabel(b)).toBe("== Schema Information · schema · 1 column · 4 lines");
  });

  it("refuses a range the document cannot hold", () => {
    expect(foldRangeIsValid(20, 1, 13)).toBe(true);
    expect(foldRangeIsValid(20, 1, 21)).toBe(false);
    expect(foldRangeIsValid(20, 0, 5)).toBe(false);
    expect(foldRangeIsValid(20, 6, 5)).toBe(false);
    expect(foldRangeIsValid(20, 1.5, 5)).toBe(false);
  });
});
