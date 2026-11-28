// HUBHOST — the phone launch sheet's height rule, pinned as source (jsdom has
// no layout). Node environment on purpose: it reads the CSS file from disk the
// way terminalCss.test.ts does.

import { describe, expect, it } from "vitest";

// eslint-disable-next-line @typescript-eslint/no-explicit-any
const fs = (globalThis as any).process.getBuiltinModule("node:fs") as { readFileSync(path: string, enc: "utf8"): string };
const read = (rel: string): string => fs.readFileSync(decodeURIComponent(new URL(rel, import.meta.url).pathname), "utf8");
const css = read("./LaunchDialog.css");

describe("phone Sheet height (HUBHOST)", () => {
  it("the launch sheet is clamped in dynamic viewport units, not static vh", () => {
    const rule = /#kb-launch-sheet\.kb-sheet\s*\{[^}]*\}/.exec(css)?.[0] ?? "";
    expect(rule).toMatch(/height:\s*min\(var\(--sheet-h, 92dvh\), 92dvh\)/);
    expect(rule).toMatch(/max-height:\s*100dvh/);
  });
});
