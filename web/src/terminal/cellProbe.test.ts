// @vitest-environment jsdom
import { afterEach, describe, expect, it, vi } from "vitest";
import { FALLBACK_CELL, measureCellRatio } from "./cellProbe";

afterEach(() => {
  vi.restoreAllMocks();
});

describe("measureCellRatio", () => {
  it("answers per px of font size from the probe's box, in the requested family", () => {
    let family = "";
    vi.spyOn(Element.prototype, "getBoundingClientRect").mockImplementation(function (this: HTMLElement) {
      family = this.style.fontFamily;
      // 20 chars at 100px: 1200 wide (0.6 per px), 132 tall (1.32 per px).
      return { width: 1200, height: 132 } as DOMRect;
    });
    const r = measureCellRatio(document, '"JetBrains Mono", monospace');
    expect(r.cellW).toBeCloseTo(0.6, 5);
    expect(r.cellH).toBeCloseTo(1.32, 5);
    expect(family).toContain("JetBrains Mono");
  });

  it("removes its probe and answers the fallback when nothing can be measured", () => {
    const before = document.body.childElementCount;
    // jsdom lays nothing out: a zero box.
    expect(measureCellRatio(document, "monospace")).toEqual(FALLBACK_CELL);
    expect(document.body.childElementCount).toBe(before);
  });
});
