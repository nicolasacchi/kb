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
