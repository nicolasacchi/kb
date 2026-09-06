import { describe, expect, it } from "vitest";
import { matchesRefinement, parseRefinement, refineRows } from "./refine";

const ROWS = [
  "app/models/order.rb",
  "app/models/Order_Presenter.rb",
  "spec/models/order_spec.rb",
  "lib/tasks/import.rake",
];

describe("refine — orderless narrowing of a page the server already chose", () => {
  it("matches space-separated needles in any order", () => {
    const { kept } = refineRows(ROWS, "models order", (r) => r);
    expect(kept).toEqual([
      "app/models/order.rb",
      "app/models/Order_Presenter.rb",
      "spec/models/order_spec.rb",
    ]);
    const { kept: reversed } = refineRows(ROWS, "order models", (r) => r);
    expect(reversed).toEqual(kept);
  });

  it("is smartcase: lowercase folds, any uppercase does not", () => {
    expect(refineRows(ROWS, "order", (r) => r).kept).toHaveLength(3);
    expect(refineRows(ROWS, "Order", (r) => r).kept).toEqual(["app/models/Order_Presenter.rb"]);
  });

  it("excludes with a leading bang", () => {
    const { kept } = refineRows(ROWS, "order !spec", (r) => r);
    expect(kept).toEqual(["app/models/order.rb", "app/models/Order_Presenter.rb"]);
  });

  /// Refinement REMOVES rows from a page; it never re-orders one. If this
  /// ever stops holding, the module has quietly become a second matcher —
  /// exactly what kbcq/1's one-matcher rule forbids.
  it("preserves the server's order and never adds a row", () => {
    const { kept } = refineRows(ROWS, "rb", (r) => r);
    const positions = kept.map((r) => ROWS.indexOf(r));
    expect(positions).toEqual([...positions].sort((a, b) => a - b));
    expect(kept.every((r) => ROWS.includes(r))).toBe(true);
  });

  it("an empty or punctuation-only refinement shows the whole page", () => {
    expect(refineRows(ROWS, "", (r) => r).kept).toBe(ROWS);
    expect(refineRows(ROWS, "   ", (r) => r).kept).toBe(ROWS);
    expect(refineRows(ROWS, "!", (r) => r).kept).toBe(ROWS);
    expect(parseRefinement("  ").empty).toBe(true);
  });

  it("is literal, never regex — a metacharacter matches itself", () => {
    const rows = ["a.b", "axb"];
    expect(refineRows(rows, "a.b", (r) => r).kept).toEqual(["a.b"]);
    expect(matchesRefinement("axb", parseRefinement("a.b"))).toBe(false);
  });

  it("matches against whatever haystack the caller builds", () => {
    const hits = [
      { path: "a.rb", line: "def call" },
      { path: "b.rb", line: "def perform" },
    ];
    const { kept } = refineRows(hits, "perform", (h) => `${h.path} ${h.line}`);
    expect(kept.map((h) => h.path)).toEqual(["b.rb"]);
  });
});
