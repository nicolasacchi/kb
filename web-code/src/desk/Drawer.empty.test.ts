import { createElement as h } from "react";
import { renderToStaticMarkup } from "react-dom/server";
import { describe, expect, it } from "vitest";
import Drawer from "./Drawer";
import { initialDrawerSets } from "./drawerSets";

// OL-C3 — an empty drawer keeps its landmark but stops narrating its
// emptiness in the tab bar. The expanded body's own EmptyState is not the
// resting caption and is covered by its owner.
describe("Drawer with no result sets", () => {
  const html = renderToStaticMarkup(
    h(Drawer, {
      state: initialDrawerSets,
      dispatch: () => {},
      collapsed: true,
      onExpand: () => {},
      onCollapse: () => {},
      onOpenRow: () => {},
    }),
  );

  it("keeps the drawer region landmark", () => {
    expect(html).toContain('data-region="drawer"');
  });

  it("renders no resting caption in the tab bar", () => {
    expect(html).not.toContain("no result sets");
    expect(html).not.toContain("keep one from a references popup");
    expect(html).not.toContain("kbc-desk__drawer-empty-tab");
  });
});
