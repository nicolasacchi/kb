import { describe, expect, it } from "vitest";
import {
  buildGalleryClusters,
  GALLERY_VIEWS,
  hasArrange,
  parseGalleryView,
  type GalleryControlsArgs,
} from "./galleryControls";

const base: GalleryControlsArgs = {
  view: "grid",
  sort: "recent",
  dir: "desc",
  group: "none",
  density: "comfy",
  onView: () => {},
  onSort: () => {},
  onGroup: () => {},
  onDensity: () => {},
};

describe("parseGalleryView", () => {
  it("accepts every ?view= value and falls back to the Shelf", () => {
    for (const v of GALLERY_VIEWS) expect(parseGalleryView(v.value)).toBe(v.value);
    expect(parseGalleryView(null)).toBe("grid");
    expect(parseGalleryView("nonsense")).toBe("grid");
  });
});

describe("buildGalleryClusters", () => {
  it("always leads with a labelled View choice listing all six views by name", () => {
    const view = buildGalleryClusters({ ...base, view: "atlas" })[0];
    expect(view.label).toBe("View");
    expect(view.controls[0].options?.map((o) => o.label)).toEqual([
      "Front page",
      "Shelf",
      "Index",
      "Atlas",
      "Timeline",
      "Canvas",
    ]);
    expect(view.controls[0].options?.map((o) => o.value)).toEqual([
      "press",
      "grid",
      "list",
      "atlas",
      "history",
      "canvas",
    ]);
  });

  it("adds Arrange and Density only on the Shelf and the Index", () => {
    for (const v of GALLERY_VIEWS) {
      const labels = buildGalleryClusters({ ...base, view: v.value }).map((c) => c.label);
      expect(labels).toEqual(hasArrange(v.value) ? ["View", "Arrange", "Density"] : ["View"]);
    }
  });

  it("routes selections to the callbacks, resetting direction to the key's default", () => {
    const seen: unknown[] = [];
    const c = buildGalleryClusters({
      ...base,
      onView: (v) => seen.push(["view", v]),
      onSort: (s, d) => seen.push(["sort", s, d]),
      onGroup: (g) => seen.push(["group", g]),
      onDensity: () => seen.push(["density"]),
    });
    c[0].controls[0].onSelect?.("list");
    c[1].controls[0].onSelect?.("title");
    c[1].controls[1].onSelect?.("folder");
    c[2].controls[0].onSelect?.();
    expect(seen).toEqual([["view", "list"], ["sort", "title", "asc"], ["group", "folder"], ["density"]]);
  });
});
