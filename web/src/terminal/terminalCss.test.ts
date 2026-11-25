// FUTERMPOLISH — the layout rules the panel's behaviour relies on, pinned as
// source (jsdom has no layout; the real boxes are measured by the e2e specs).

import { describe, expect, it } from "vitest";

// eslint-disable-next-line @typescript-eslint/no-explicit-any
const fs = (globalThis as any).process.getBuiltinModule("node:fs") as { readFileSync(path: string, enc: "utf8"): string };
const read = (rel: string): string => fs.readFileSync(decodeURIComponent(new URL(rel, import.meta.url).pathname), "utf8");
const css = read("./terminal.css");

function rule(sel: string): string {
  const i = css.indexOf(`${sel} {`);
  if (i < 0) throw new Error(`no rule for ${sel}`);
  return css.slice(i, css.indexOf("}", i));
}

describe("terminal.css", () => {
  it("W2: the inline view is bounded by the window below the app bar, not by 80% of it", () => {
    const view = rule(".kb-term__view");
    expect(view).toMatch(/height:\s*max\(/);
    expect(view).toContain("100dvh");
    expect(view).toContain("--topbar-h");
    expect(view).not.toContain("80dvh");
  });

  it("W2: the panel's top stops under the app bar when scrolled into view", () => {
    expect(rule(".kb-term")).toBeTruthy();
    expect(css).toMatch(/\.kb-term \{\s*scroll-margin-top:/);
  });

  it("W5: the focus ring and the scrollbar sit on the grid's box (the xterm element shrink-wraps its screen), not the host", () => {
    expect(rule(".kb-term__view .xterm")).toContain("width: max-content");
    const typing = rule(".kb-term--typing .kb-term__view .xterm");
    expect(typing).toContain("outline");
    expect(css).not.toMatch(/\.kb-term--typing \.kb-term__view \{/);
  });
});

describe("BHEAD header css contract", () => {
  it("the chips track shrinks and clips instead of wrapping or growing the row", () => {
    const chips = rule(".kb-term__chips");
    expect(chips).toContain("flex-wrap: nowrap");
    expect(chips).toContain("overflow: hidden");
    expect(chips).toContain("min-width: 0");
    expect(rule(".kb-term__bar")).toContain("flex-wrap: nowrap");
  });

  it("the pulse is removed under prefers-reduced-motion", () => {
    expect(css).toMatch(/prefers-reduced-motion: reduce\) \{\s*\.kb-term__state \.kb-term__dot \{\s*animation: none/);
  });

  it("coarse pointers get 40px header targets", () => {
    const i = css.indexOf("@media (pointer: coarse)");
    expect(i).toBeGreaterThan(0);
    const block = css.slice(i);
    expect(block).toContain("min-height: 40px");
    expect(block).toContain("min-width: 40px");
  });
});

describe("BSTATE state overlay css contract", () => {
  const region = css.slice(css.indexOf("/* BSTATE — the state overlay"));

  it("sits over the pane box, takes no pointer events itself, and the pane adds no size", () => {
    const overlay = rule(".kb-term-state-overlay");
    expect(overlay).toContain("position: absolute");
    expect(overlay).toContain("inset: 0");
    expect(overlay).toContain("pointer-events: none");
    const pane = rule(".kb-term__pane");
    expect(pane).toContain("position: relative");
    expect(pane).not.toMatch(/\b(height|width|padding)\s*:/);
  });

  it("uses tokens only: no hex or rgb colour, no px font size, no z-index literal", () => {
    expect(region).not.toMatch(/#[0-9a-fA-F]{3,8}\b/);
    expect(region).not.toMatch(/rgba?\(/);
    expect(region).not.toMatch(/font-size:\s*[0-9.]+(px|rem|em)/);
    for (const m of region.matchAll(/font-size:\s*([^;]+);/g)) expect(m[1]).toMatch(/^var\(--fs-/);
    for (const m of region.matchAll(/z-index:\s*([^;]+);/g)) expect(m[1]).toMatch(/^var\(--z-/);
  });

  it("the overlay pulse is switched off under prefers-reduced-motion", () => {
    expect(css).toMatch(/prefers-reduced-motion: reduce\) \{[^@]*\.kb-term-state-overlay__dot \{\s*animation: none/);
  });

  it("the action is a 44px target on coarse pointers", () => {
    expect(css).toMatch(/pointer: coarse\) \{\s*\.kb-term-state-overlay__action \{\s*min-height: 44px/);
  });
});
