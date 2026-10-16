// @vitest-environment jsdom
import { describe, expect, it } from "vitest";
import { renderToString } from "react-dom/server";
import SearchViewBar from "./SearchViewBar";
import { buildSearchClusters, NO_MODEL_REASON, type SearchControlsArgs } from "../../lib/searchControls";

const base: SearchControlsArgs = {
  mode: "hybrid",
  scope: "one",
  limit: 50,
  sort: "relevance",
  noModel: false,
  onMode: () => {},
  onScope: () => {},
  onLimit: () => {},
  onSort: () => {},
};
const render = (noModel: boolean) =>
  renderToString(
    <SearchViewBar
      clusters={buildSearchClusters({ ...base, noModel })}
      noModel={noModel}
      renderSort={() => <select aria-label="sort" className="sort-control__select" />}
    />,
  );

describe("SearchViewBar", () => {
  it("has no title legend and no inner duplicate label; the toolbar keeps its aria-label", () => {
    const html = render(false);
    expect(html).toContain('aria-label="Search view"');
    expect(html).not.toContain("kb-viewbar__title");
    expect(html).not.toContain("kb-viewbar__field-label");
    expect(html).toContain('aria-label="Results"');
    const legends = [...html.matchAll(/kb-viewbar__legend">([^<]+)</g)].map((m) => m[1]);
    expect(legends).toEqual(["Mode", "Scope", "Results", "Sort"]);
    expect(html).not.toContain("kb-search__viewbar-note");
  });

  it("puts the no-model caption in ONE line under the bar, not between the buttons", () => {
    const root = document.createElement("div");
    root.innerHTML = render(true);
    const toolbar = root.querySelector("[data-kb-viewbar]")!;
    const notes = root.querySelectorAll(".kb-search__viewbar-note");
    expect(notes.length).toBe(1);
    expect(notes[0].textContent).toBe(NO_MODEL_REASON);
    // after the toolbar in document order, and not inside it
    expect(toolbar.contains(notes[0])).toBe(false);
    expect(toolbar.compareDocumentPosition(notes[0]) & Node.DOCUMENT_POSITION_FOLLOWING).toBeTruthy();
    // no caption element between the segment buttons; the disabled group carries the title
    expect(toolbar.querySelector(".kb-viewbar__reason")).toBeNull();
    expect(root.querySelector(`[title="${NO_MODEL_REASON}"] [data-kb-viewbar-control="mode-model"]`)).not.toBeNull();
  });
});
