import { createElement, type ComponentType } from "react";
import { renderToString } from "react-dom/server";
import { describe, expect, it } from "vitest";
import { DEFAULT_ICON_SIZES, Icon, createIconSet } from "./icons";

const EXPECTED_NAMES = [
  "Anchor",
  "Arrow",
  "ArrowLeft",
  "Atlas",
  "BookOpen",
  "Bookmark",
  "Brain",
  "Branch",
  "BranchPlus",
  "Bridge",
  "Check",
  "ChevDown",
  "Chevron",
  "ClipboardCheck",
  "Clock",
  "Cmd",
  "Code",
  "Collapse",
  "Comment",
  "Contrast",
  "Copy",
  "Doc",
  "Dot",
  "Download",
  "Entity",
  "Expand",
  "External",
  "Eye",
  "Facts",
  "File",
  "Flame",
  "Folder",
  "Fork",
  "Graph",
  "Grid",
  "History",
  "ImageFrame",
  "Inbox",
  "Layers",
  "Link",
  "List",
  "Mark",
  "Moon",
  "More",
  "Note",
  "OpenInReader",
  "Palette",
  "Panel",
  "Paperclip",
  "Pause",
  "Pen",
  "Pin",
  "Play",
  "Plus",
  "Press",
  "PullRequest",
  "Record",
  "Refresh",
  "Search",
  "Settings",
  "Share",
  "Slate",
  "Spark",
  "SplitView",
  "Star",
  "Sun",
  "Swap",
  "Table",
  "Tasks",
  "Terminal",
  "TerminalHub",
  "ThumbDown",
  "ThumbUp",
  "Trash",
  "Type",
  "UnifiedView",
  "Unlink",
  "Warn",
  "X",
];

describe("ui/icons", () => {
  it("exports exactly the expected, sorted set of icon names", () => {
    expect(Object.keys(Icon).sort()).toEqual(EXPECTED_NAMES);
  });

  it("every icon renders an <svg>", () => {
    for (const [name, Glyph] of Object.entries(Icon)) {
      const html = renderToString(createElement(Glyph));
      expect(html.startsWith("<svg"), `Icon.${name} renders an svg`).toBe(true);
    }
  });

  it("forwards props to the svg (className, aria-label)", () => {
    const html = renderToString(
      createElement(Icon.Search, { className: "x", "aria-label": "find" }),
    );
    expect(html).toContain('class="x"');
    expect(html).toContain('aria-label="find"');
  });

  it("Inbox is its own tray glyph, not the hamburger List", () => {
    const html = (G: ComponentType) => renderToString(createElement(G));
    expect(html(Icon.Inbox)).not.toBe(html(Icon.List));
    expect(html(Icon.Inbox)).toContain('viewBox="0 0 16 16"');
  });

  it("TerminalHub is an svg and differs from Terminal", () => {
    const html = (G: ComponentType) => renderToString(createElement(G));
    expect(html(Icon.TerminalHub).startsWith("<svg")).toBe(true);
    expect(html(Icon.TerminalHub)).not.toBe(html(Icon.Terminal));
  });

  it("TerminalHub draws two stacked windows: a complete front rect and a back window path", () => {
    const out = renderToString(createElement(Icon.TerminalHub));
    expect(out).toContain('width="14"');
    // exactly one complete rounded window (the front one)...
    expect(out.match(/<rect /g)?.length).toBe(1);
    // ...plus the back window's visible edges, the chevron and the cursor dash.
    expect(out.match(/<path /g)?.length).toBe(3);
    expect(out).toContain("M6 4.5V2.7");
    // every coordinate stays inside the 16 grid
    for (const n of out.match(/-?\d+(?:\.\d+)?/g) ?? []) expect(Math.abs(Number(n))).toBeLessThanOrEqual(16);
  });

  it("every size-table name is a real icon", () => {
    for (const name of Object.keys(DEFAULT_ICON_SIZES)) {
      expect(Object.keys(Icon)).toContain(name);
    }
  });

  it("createIconSet overrides the default size of the named icons only", () => {
    const set = createIconSet({ Search: 16, X: 12 });
    const size = (G: ComponentType) =>
      /width="(\d+)"/.exec(renderToString(createElement(G)))?.[1];
    expect(size(Icon.Search)).toBe("14");
    expect(size(set.Search)).toBe("16");
    expect(size(Icon.X)).toBe("14");
    expect(size(set.X)).toBe("12");
    expect(size(set.Grid)).toBe(size(Icon.Grid));
  });
});
