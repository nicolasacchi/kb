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
    const html = render(true);
    const note = html.indexOf("kb-search__viewbar-note");
    expect(note).toBeGreaterThan(html.lastIndexOf("kb-viewbar__cluster"));
    expect(html.split(NO_MODEL_REASON).length - 1).toBeGreaterThanOrEqual(1);
    // the visible-caption copy is the note only; the disabled buttons carry it as title
    expect(html).toContain(`title="Model modes - ${NO_MODEL_REASON}"`);
    // the note sits after the toolbar closes
    const toolbarEnd = html.indexOf("</div></div>", html.lastIndexOf("kb-viewbar__cluster"));
    expect(note).toBeGreaterThan(toolbarEnd);
  });
});
