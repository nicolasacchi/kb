import { describe, expect, it } from "vitest";
import {
  emptyCollapseTick,
  fileCollapse,
  formatExpandedParam,
  hunkCollapse,
  parseExpandedParam,
  reduceCollapseTick,
  toggleSectionCollapse,
} from "./collapseOnTick";

describe("hunkCollapse", () => {
  it("viewed and not expanded → collapsed by viewed", () => {
    expect(hunkCollapse({ viewed: true, folded: false, byNoise: false, expanded: false })).toEqual({
      collapsed: true,
      collapsedBy: "viewed",
    });
  });

  it("viewed + expanded override → open (unless folded/noise)", () => {
    expect(hunkCollapse({ viewed: true, folded: false, byNoise: false, expanded: true })).toEqual({
      collapsed: false,
      collapsedBy: null,
    });
  });

  it("fold wins over viewed", () => {
    expect(hunkCollapse({ viewed: true, folded: true, byNoise: false, expanded: true })).toEqual({
      collapsed: true,
      collapsedBy: "fold",
    });
  });

  it("noise wins over viewed, loses to fold", () => {
    expect(hunkCollapse({ viewed: true, folded: false, byNoise: true, expanded: false })).toEqual({
      collapsed: true,
      collapsedBy: "noise",
    });
    expect(hunkCollapse({ viewed: false, folded: true, byNoise: true, expanded: false })).toEqual({
      collapsed: true,
      collapsedBy: "fold",
    });
  });

  it("unviewed + not folded → open", () => {
    expect(hunkCollapse({ viewed: false, folded: false, byNoise: false, expanded: false })).toEqual({
      collapsed: false,
      collapsedBy: null,
    });
  });
});

describe("fileCollapse", () => {
  it("ticking viewed collapses the file", () => {
    expect(fileCollapse({ viewed: true, userCollapsed: false, expanded: false })).toEqual({
      collapsed: true,
      collapsedBy: "viewed",
    });
  });

  it("the chevron (`userCollapsed`) stays independent", () => {
    expect(fileCollapse({ viewed: false, userCollapsed: true, expanded: false })).toEqual({
      collapsed: true,
      collapsedBy: "fold",
    });
    expect(fileCollapse({ viewed: true, userCollapsed: true, expanded: true })).toEqual({
      collapsed: true,
      collapsedBy: "fold",
    });
  });
});

describe("toggleSectionCollapse", () => {
  it("expanding a viewed section records the override", () => {
    expect(toggleSectionCollapse({ collapsed: true, viewed: true, expanded: false })).toEqual({
      expanded: true,
      userCollapsed: false,
    });
  });

  it("collapsing an open section sets the user fold", () => {
    expect(toggleSectionCollapse({ collapsed: false, viewed: true, expanded: true })).toEqual({
      expanded: false,
      userCollapsed: true,
    });
  });
});

describe("reduceCollapseTick", () => {
  it("markViewed drops the expand override so the tick collapses", () => {
    const s = reduceCollapseTick(emptyCollapseTick(), {
      type: "set",
      files: ["a.ts"],
      hunks: ["deadbeefdeadbeef"],
    });
    const file = reduceCollapseTick(s, { type: "markViewed", kind: "file", id: "a.ts" });
    expect(file.expandedFiles.has("a.ts")).toBe(false);
    const hunk = reduceCollapseTick(s, { type: "markViewed", kind: "hunk", id: "deadbeefdeadbeef" });
    expect(hunk.expandedHunks.has("deadbeefdeadbeef")).toBe(false);
  });

  it("collapseAllViewed clears overrides for the viewed set", () => {
    const s = reduceCollapseTick(emptyCollapseTick(), {
      type: "set",
      files: ["a.ts", "b.ts"],
      hunks: ["aaaa", "bbbb"],
    });
    const next = reduceCollapseTick(s, {
      type: "collapseAllViewed",
      fileIds: ["a.ts"],
      hunkIds: ["aaaa"],
    });
    expect([...next.expandedFiles]).toEqual(["b.ts"]);
    expect([...next.expandedHunks]).toEqual(["bbbb"]);
  });

  it("expandAll adds every id", () => {
    const next = reduceCollapseTick(emptyCollapseTick(), {
      type: "expandAll",
      fileIds: ["a.ts"],
      hunkIds: ["aaaa"],
    });
    expect(next.expandedFiles.has("a.ts")).toBe(true);
    expect(next.expandedHunks.has("aaaa")).toBe(true);
  });

  it("toggleSection on a viewed-collapsed hunk records the override", () => {
    const next = reduceCollapseTick(emptyCollapseTick(), {
      type: "toggleSection",
      kind: "hunk",
      id: "aaaa",
      viewed: true,
      collapsed: true,
    });
    expect(next.expandedHunks.has("aaaa")).toBe(true);
  });
});

describe("expanded URL param", () => {
  it("omits the empty set", () => {
    expect(formatExpandedParam([])).toBeNull();
  });

  it("round-trips paths with slashes and spaces", () => {
    const raw = formatExpandedParam(["src/a.ts", "has space.rs"]);
    expect(raw).toBe("has%20space.rs,src%2Fa.ts");
    expect(parseExpandedParam(raw)).toEqual(["has space.rs", "src/a.ts"]);
  });

  it("is TOTAL — null, empty, and extra commas are []", () => {
    expect(parseExpandedParam(null)).toEqual([]);
    expect(parseExpandedParam("")).toEqual([]);
    expect(parseExpandedParam(",,a,,")).toEqual(["a"]);
  });

  it("keeps a broken percent-encoding rather than throwing", () => {
    expect(parseExpandedParam("%zz")).toEqual(["%zz"]);
  });
});
