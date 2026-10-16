// @vitest-environment jsdom
import { afterEach, describe, expect, it, vi } from "vitest";
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
    // two tag chips; the folder is named by the header breadcrumb, not a chip
    expect(c.container.querySelectorAll(".kb-fchip").length).toBe(2);
    expect(c.container.querySelector(".kb-gallery-viewbar__filters")?.textContent).not.toContain("x/y");
    expect(c.container.querySelector(".kb-gallery-viewbar__warn")?.getAttribute("title")).toBe("bad token");
    fireEvent.click(c.getByLabelText("remove a"));
    expect(search(c)).toContain("tags=b");
    fireEvent.click(c.getByLabelText("remove b"));
    expect(search(c)).not.toContain("tags=");
  });

  const withProps = (props: { shown?: number; notes?: boolean }, onToggle = vi.fn()) =>
    render(
      <MemoryRouter initialEntries={["/"]}>
        <QueryStatsProvider>
          <Report />
          <GalleryViewBar
            scope="canon"
            chromeless={false}
            notesOpen
            onToggleNotes={props.notes === false ? undefined : onToggle}
            shown={props.shown}
          />
        </QueryStatsProvider>
      </MemoryRouter>,
    );

  it("carries Notes as a real ViewBar control: the last cluster, inside the toolbar, with its class and aria-pressed", () => {
    const onToggle = vi.fn();
    const c = withProps({}, onToggle);
    const btn = c.container.querySelector<HTMLButtonElement>(
      '[data-kb-viewbar] [data-kb-viewbar-cluster]:last-of-type .gallery-notes-toggle',
    )!;
    expect(btn).not.toBeNull();
    expect(btn.getAttribute("aria-pressed")).toBe("true");
    expect(btn.getAttribute("title")).toBe("notes for the current folder / kb");
    fireEvent.click(btn);
    expect(onToggle).toHaveBeenCalledTimes(1);
    expect(c.container.querySelector(".kb-gallery-viewbar__row")).toBeNull();
  });

  it("says 'M matches' when everything is loaded and 'showing N of M' under a partial page", () => {
    expect(withProps({ shown: 1234 }).container.querySelector(".kb-viewbar__count")?.textContent).toBe("1,234 matches · 7 ms");
    cleanup();
    expect(withProps({ shown: 200 }).container.querySelector(".kb-viewbar__count")?.textContent).toBe("showing 200 of 1,234 · 7 ms");
  });

  it("has no filter line when nothing is filtered, and hides stats on chromeless views", () => {
    expect(mount("/").container.querySelector(".kb-gallery-viewbar__filters")).toBeNull();
    cleanup();
    const c = mount("/?view=history&tags=a", { chromeless: true });
    expect(c.container.querySelector(".kb-viewbar__count")).toBeNull();
    expect(c.container.querySelector(".kb-gallery-viewbar__filters")).toBeNull();
  });
});
