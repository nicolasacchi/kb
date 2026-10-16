// @vitest-environment jsdom
import { afterEach, describe, expect, it } from "vitest";
import { cleanup, fireEvent, render } from "@testing-library/react";
import { MemoryRouter, useLocation } from "react-router-dom";
import GalleryViewBar from "./GalleryViewBar";
import { QueryStatsProvider, useReportQueryStats } from "./chrome/queryStats";

afterEach(cleanup);

const stats = { total: 1234, ms: 7, warnings: [] as string[] };

function Report({ warn = false }: { warn?: boolean }) {
  useReportQueryStats({ ...stats, warnings: warn ? ["bad token"] : [] });
  return null;
}
function Loc() {
  return <output data-testid="loc">{useLocation().search}</output>;
}

function mount(url: string, opts: { warn?: boolean; chromeless?: boolean } = {}) {
  return render(
    <MemoryRouter initialEntries={[url]}>
      <QueryStatsProvider>
        <Report warn={opts.warn} />
        <GalleryViewBar scope="canon" chromeless={opts.chromeless ?? false} />
        <Loc />
      </QueryStatsProvider>
    </MemoryRouter>,
  );
}

const search = (c: ReturnType<typeof render>) => c.getByTestId("loc").textContent;

describe("GalleryViewBar", () => {
  it("names the scope, the match count and a labelled View choice", () => {
    const c = mount("/");
    expect(c.container.querySelector(".kb-viewbar__title")?.textContent).toBe("canon");
    expect(c.container.querySelector(".kb-viewbar__count")?.textContent).toBe("1,234 matches · 7 ms");
    const legends = Array.from(c.container.querySelectorAll(".kb-viewbar__legend")).map((e) => e.textContent);
    expect(legends).toEqual(["View", "Arrange", "Density"]);
    const sel = c.container.querySelector<HTMLSelectElement>('[data-kb-viewbar-control="view"]')!;
    expect(sel.value).toBe("grid");
    expect(sel.getAttribute("aria-label")).toBe("View");
    expect(Array.from(sel.options).map((o) => o.textContent)).toContain("Timeline");
    // the e2e-pinned sort select survives
    expect(c.container.querySelector(".sort-control__select")).not.toBeNull();
  });

  it("switches views through ?view= and drops Arrange where it means nothing", () => {
    const c = mount("/?kb=canon&sort=title");
    const sel = () => c.container.querySelector<HTMLSelectElement>('[data-kb-viewbar-control="view"]')!;
    fireEvent.change(sel(), { target: { value: "atlas" } });
    expect(search(c)).toContain("view=atlas");
    expect(search(c)).toContain("kb=canon");
    expect(c.container.querySelector(".sort-control__select")).toBeNull();
    fireEvent.change(sel(), { target: { value: "grid" } });
    expect(search(c)).not.toContain("view=");
    expect(c.container.querySelector(".sort-control__select")).not.toBeNull();
  });

  it("sorts with the key's default direction", () => {
    const c = mount("/?sort=words&dir=asc");
    fireEvent.change(c.container.querySelector(".sort-control__select")!, { target: { value: "title" } });
    expect(search(c)).toContain("sort=title");
    expect(search(c)).not.toContain("dir=");
  });

  it("renders active filters as removable chips and shows query warnings", () => {
    const c = mount("/?tags=a,b&folder=x/y&view=list", { warn: true });
    expect(c.container.querySelectorAll(".kb-fchip").length).toBe(3);
    expect(c.container.querySelector(".kb-gallery-viewbar__warn")?.getAttribute("title")).toBe("bad token");
    fireEvent.click(c.getByLabelText("remove a"));
    expect(search(c)).toContain("tags=b");
    fireEvent.click(c.getByLabelText("remove x/y"));
    expect(search(c)).not.toContain("folder=");
  });

  it("has no filter line when nothing is filtered, and hides stats on chromeless views", () => {
    expect(mount("/").container.querySelector(".kb-gallery-viewbar__filters")).toBeNull();
    cleanup();
    const c = mount("/?view=history&tags=a", { chromeless: true });
    expect(c.container.querySelector(".kb-viewbar__count")).toBeNull();
    expect(c.container.querySelector(".kb-gallery-viewbar__filters")).toBeNull();
  });
});
