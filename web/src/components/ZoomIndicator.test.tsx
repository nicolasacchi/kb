import { createElement } from "react";
import { renderToString } from "react-dom/server";
import { describe, expect, it } from "vitest";
import ZoomIndicator from "./ZoomIndicator";

const html = (view: string) => renderToString(createElement(ZoomIndicator, { view }));

describe("ZoomIndicator", () => {
  it("fills exactly the tick of the current view", () => {
    for (const [view, label] of [["atlas", "Atlas"], ["list", "Index"], ["grid", "Shelf"]] as const) {
      const out = html(view);
      expect(out).toContain(`aria-label="Library zoom: ${label}"`);
      expect((out.match(/is-on/g) ?? []).length).toBe(1);
      expect(out).toMatch(new RegExp(`is-on" data-zoom-tick="${view}"`));
      expect((out.match(/data-zoom-tick/g) ?? []).length).toBe(3);
    }
  });

  it("renders nothing off the ladder", () => {
    for (const v of ["history", "canvas", "press"]) expect(html(v)).toBe("");
  });
});
