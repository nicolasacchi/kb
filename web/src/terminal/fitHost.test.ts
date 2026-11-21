// @vitest-environment jsdom
import { afterEach, describe, expect, it, vi } from "vitest";
import { fitHostNow } from "./fitHost";

afterEach(() => {
  vi.restoreAllMocks();
});

function hostWith(width: number, height: number, screen?: { w: number; h: number }): HTMLElement {
  const host = document.createElement("div");
  Object.defineProperty(host, "clientWidth", { value: width, configurable: true });
  Object.defineProperty(host, "clientHeight", { value: height, configurable: true });
  if (screen) {
    const el = document.createElement("div");
    el.className = "xterm-screen";
    Object.defineProperty(el, "scrollWidth", { get: () => screen.w, configurable: true });
    Object.defineProperty(el, "scrollHeight", { get: () => screen.h, configurable: true });
    host.appendChild(el);
  }
  return host;
}

// A 0.6 x 1.32 cell, as the probe would measure it.
const probe = () =>
  vi.spyOn(Element.prototype, "getBoundingClientRect").mockReturnValue({ width: 1200, height: 132 } as DOMRect);

describe("fitHostNow", () => {
  it("never fits while the host has no width (a closed <details>), and leaves the font alone", () => {
    const term = { options: { fontSize: 14, fontFamily: "monospace" } };
    expect(fitHostNow(term, hostWith(0, 600), { cols: 201, rows: 48 }, { min: 9, max: 20 })).toBeNull();
    expect(term.options.fontSize).toBe(14);
  });

  it("applies the font from the probed cell and reports an uncropped fit", () => {
    probe();
    const term = { options: { fontSize: 10, fontFamily: "monospace" } };
    const r = fitHostNow(term, hostWith(1400, 800, { w: 1300, h: 700 }), { cols: 201, rows: 48 }, { min: 9, max: 20 });
    expect(term.options.fontSize).toBe(11);
    expect(r).toEqual({ fontSize: 11, cropped: { x: false, y: false } });
  });

  it("steps down while the rendered screen still overflows, then reports the crop and scrolls", () => {
    probe();
    const term = { options: { fontSize: 14, fontFamily: "monospace" } };
    const host = hostWith(800, 800, { w: 1500, h: 700 });
    const r = fitHostNow(term, host, { cols: 201, rows: 48 }, { min: 9, max: 20 });
    expect(term.options.fontSize).toBe(9);
    expect(r?.cropped).toEqual({ x: true, y: false });
    expect(host.style.overflow).toBe("auto");
  });

  it("an uncropped fit clears the scroll", () => {
    probe();
    const term = { options: { fontSize: 14, fontFamily: "monospace" } };
    const host = hostWith(1400, 800, { w: 1300, h: 700 });
    host.style.overflow = "auto";
    fitHostNow(term, host, { cols: 201, rows: 48 }, { min: 9, max: 20 });
    expect(host.style.overflow).toBe("");
  });
});

describe("fitHostNow in a host as tall as its content", () => {
  // The painted cell height is the probed one rounded DOWN to a whole pixel, and
  // the host has no height of its own: it is exactly as tall as the screen.
  function contentSized(term: { options: { fontSize?: number } }, rows: number) {
    const host = document.createElement("div");
    const screen = document.createElement("div");
    screen.className = "xterm-screen";
    const height = () => rows * Math.floor((term.options.fontSize ?? 14) * 1.32);
    Object.defineProperty(screen, "scrollWidth", { get: () => 400, configurable: true });
    Object.defineProperty(screen, "scrollHeight", { get: height, configurable: true });
    Object.defineProperty(host, "clientWidth", { value: 1400, configurable: true });
    Object.defineProperty(host, "clientHeight", { get: height, configurable: true });
    host.appendChild(screen);
    return host;
  }

  it("a font that already fits is kept: repeated refits do not ratchet it down to the floor", () => {
    probe();
    const term = { options: { fontSize: 14, fontFamily: "monospace" } };
    const host = contentSized(term, 24);
    for (let i = 0; i < 8; i += 1) fitHostNow(term, host, { cols: 80, rows: 24 }, { min: 9, max: 20 });
    expect(term.options.fontSize).toBe(14);
  });

  it("a font that does not fit is still shrunk to the plan", () => {
    probe();
    const term = { options: { fontSize: 20, fontFamily: "monospace" } };
    const host = hostWith(1400, 400, { w: 1000, h: 700 });
    fitHostNow(term, host, { cols: 80, rows: 24 }, { min: 9, max: 20 });
    expect(term.options.fontSize).toBeLessThan(20);
  });
});

describe("fitHostNow: a new situation is planned from scratch (FUTERMPOLISH W3)", () => {
  it("fresh: a larger font that still fits is NOT kept over the plan (a grid that grew needs the smaller font)", () => {
    probe();
    const term = { options: { fontSize: 18, fontFamily: "monospace" } };
    // The screen painted so far fits the host, so the no-ratchet guard would keep 18.
    const host = hostWith(1230, 576, { w: 800, h: 400 });
    const kept = fitHostNow({ options: { ...term.options } }, host, { cols: 201, rows: 48 }, { min: 9, max: 20 });
    expect(kept?.fontSize).toBe(18);
    const fresh = fitHostNow(term, host, { cols: 201, rows: 48 }, { min: 9, max: 20, fresh: true });
    expect(fresh?.fontSize).toBe(9);
    expect(term.options.fontSize).toBe(9);
  });

  it("the font returned is the font applied, also when the step-down loop runs out of passes", () => {
    probe();
    const term = { options: { fontSize: 20, fontFamily: "monospace" } };
    // A screen that never fits: the loop steps down at most four times, and the result is applied.
    const host = hostWith(1230, 576, { w: 99999, h: 99999 });
    const r = fitHostNow(term, host, { cols: 80, rows: 24 }, { min: 9, max: 20, fresh: true });
    expect(term.options.fontSize).toBe(r?.fontSize);
  });
});
