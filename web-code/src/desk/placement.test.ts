import { describe, expect, it } from "vitest";
import { PLACEMENT_RULES, placementFor, placementTableLines, type ContentKind } from "./placement";
import { RAIL_TABS } from "./deskState";

const ALL_KINDS: ContentKind[] = [
  "file-open",
  "definition-single",
  "definition-multi",
  "usages",
  "search-results",
  "diagnostics",
  "recipe-output",
  "peek",
  "blame",
  "review-thread",
  "annotation",
  "symbol-info",
  "companion",
  "pr-diff",
  "canvas",
];

describe("placement rule table", () => {
  it("covers every content kind exactly once", () => {
    const kinds = PLACEMENT_RULES.map((r) => r.kind);
    expect([...kinds].sort()).toEqual([...ALL_KINDS].sort());
    expect(new Set(kinds).size).toBe(kinds.length);
  });

  it("every rail home names a real rail tab", () => {
    for (const r of PLACEMENT_RULES) {
      if (r.home.region === "rail") expect(RAIL_TABS).toContain(r.home.tab);
    }
  });

  it("every rule carries a human note", () => {
    for (const r of PLACEMENT_RULES) expect(r.note.length).toBeGreaterThan(0);
  });

  it("the shipped census is exactly the rows a consumer reads today", () => {
    // Flipping a row to `shipped: true` is a deliberate act: its
    // consumer has to land in the same commit that changes this list.
    const shipped = PLACEMENT_RULES.filter((r) => r.shipped).map((r) => r.kind).sort();
    expect(shipped).toEqual(
      ["annotation", "blame", "definition-single", "file-open", "peek", "review-thread", "symbol-info"].sort(),
    );
  });

  it("file-open goes to the focused pane", () => {
    expect(placementFor("file-open")).toEqual({ region: "main", pane: "focused" });
  });

  it("a PINNED focused pane sends the open to the other pane", () => {
    expect(placementFor("file-open", { pinned: [1], focused: 1, paneCount: 2 })).toEqual({
      region: "main",
      pane: "other",
    });
    expect(placementFor("file-open", { pinned: [2], focused: 2, paneCount: 2 })).toEqual({
      region: "main",
      pane: "other",
    });
  });

  it("a pin on the OTHER pane changes nothing", () => {
    expect(placementFor("file-open", { pinned: [2], focused: 1, paneCount: 2 })).toEqual({
      region: "main",
      pane: "focused",
    });
  });

  it("with one pane the pin loses — opening beats refusing", () => {
    expect(placementFor("file-open", { pinned: [1], focused: 1, paneCount: 1 })).toEqual({
      region: "main",
      pane: "focused",
    });
  });

  it("a pin never redirects a non-main home", () => {
    const ctx = { pinned: [1, 2] as (1 | 2)[], focused: 1 as const, paneCount: 2 as const };
    expect(placementFor("blame", ctx)).toEqual({ region: "rail", tab: "history" });
    expect(placementFor("usages", ctx)).toEqual({ region: "drawer", set: "usages" });
    expect(placementFor("peek", ctx)).toEqual({ region: "float" });
    expect(placementFor("canvas", ctx)).toEqual({ region: "takeover" });
  });

  it("the printable table has one line per rule and flags the unrouted ones", () => {
    const lines = placementTableLines();
    expect(lines.length).toBe(PLACEMENT_RULES.length);
    expect(lines.some((l) => l.startsWith("file-open → main.focused —"))).toBe(true);
    expect(lines.some((l) => l.includes("usages → drawer:usages (declared, not yet routed)"))).toBe(true);
  });
});
