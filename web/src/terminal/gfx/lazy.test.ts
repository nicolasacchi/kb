// T15 - the laziness boundary for the graphics layer: `gfx/` and the
// lightbox are imported only from inside `web/src/terminal/`, so they ride
// the lazy terminal chunk and never reach the app shell.

import { describe, expect, it } from "vitest";

// Node's fs without Node typings (same trick as no-static-xterm.test.ts).
// eslint-disable-next-line @typescript-eslint/no-explicit-any
const nodeFs = (globalThis as any).process.getBuiltinModule("node:fs") as {
  readFileSync(path: string, enc: "utf8"): string;
  readdirSync(dir: string, opts?: { withFileTypes: boolean }): Array<{ name: string; isDirectory(): boolean }>;
};
const SRC = decodeURIComponent(new URL("../../../src/", import.meta.url).pathname);

function walk(dir: string): string[] {
  const out: string[] = [];
  for (const entry of nodeFs.readdirSync(dir, { withFileTypes: true })) {
    const full = dir + entry.name;
    if (entry.isDirectory()) out.push(...walk(full + "/"));
    else if (/\.(ts|tsx)$/.test(entry.name)) out.push(full);
  }
  return out;
}

describe("graphics layer laziness", () => {
  it("gfx is imported only from web/src/terminal", () => {
    const offenders: string[] = [];
    // `terminal/gfx`, `./gfx` (from inside terminal/) and `../terminal/gfx`.
    const gfx = /(?:from\s+|import\s*\(\s*|import\s+)["'][^"']*(?:terminal\/gfx|\/gfx)(?:\/[^"']*)?["']/;
    const lightbox = /(?:from\s+|import\s*\(\s*|import\s+)["'][^"']*terminal\/Lightbox["']/;
    for (const file of walk(SRC)) {
      if (file.includes("/terminal/")) continue;
      const text = nodeFs.readFileSync(file, "utf8");
      if (gfx.test(text)) offenders.push(`${file}: gfx`);
      if (lightbox.test(text)) offenders.push(`${file}: Lightbox`);
    }
    expect(offenders).toEqual([]);
  });

  it("Lightbox is imported only from web/src/terminal", () => {
    const importers: string[] = [];
    for (const file of walk(SRC)) {
      const text = nodeFs.readFileSync(file, "utf8");
      if (/from\s+["'](?:\.\/|\.\.\/terminal\/|@\/terminal\/)Lightbox["']/.test(text)) importers.push(file);
    }
    expect(importers.length).toBeGreaterThan(0);
    for (const f of importers) expect(f).toContain("/terminal/");
  });

  it("the walk really sees the panel importing gfx", () => {
    const panel = nodeFs.readFileSync(SRC + "terminal/TerminalPanel.tsx", "utf8");
    expect(panel).toMatch(/from\s+"\.\/gfx"/);
  });
});
