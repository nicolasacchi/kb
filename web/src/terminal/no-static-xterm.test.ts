// T14 — the lazy boundary: no static `@xterm/*` import outside
// `web/src/terminal/`, and within `terminal/` only `TerminalPanel.lazy.ts`
// references `./TerminalPanel` (via `import(`). Walks `web/src/**` with
// the getBuiltinModule fs pattern.

import { describe, expect, it } from "vitest";

// Node's fs without Node typings (same trick as lib/codeDoors.runtime.test.ts).
// eslint-disable-next-line @typescript-eslint/no-explicit-any
const nodeFs = (globalThis as any).process.getBuiltinModule("node:fs") as {
  readFileSync(path: string, enc: "utf8"): string;
  readdirSync(dir: string, opts?: { withFileTypes: boolean }): Array<{ name: string; isDirectory(): boolean }>;
};
const here = (rel: string): string =>
  decodeURIComponent(new URL(rel, import.meta.url).pathname);
const SRC = here("../../src/");

function walk(dir: string): string[] {
  const out: string[] = [];
  for (const entry of nodeFs.readdirSync(dir, { withFileTypes: true })) {
    const full = dir + entry.name;
    if (entry.isDirectory()) out.push(...walk(full + "/"));
    else if (/\.(ts|tsx)$/.test(entry.name)) out.push(full);
  }
  return out;
}

function findXtermImports(text: string): string[] {
  const found: string[] = [];
  const re = /(?:from\s+|import\s+|import\s*\(\s*)["'](@xterm\/[^"']+)["']/g;
  for (const m of text.matchAll(re)) found.push(m[1]);
  return found;
}

describe("no static xterm imports", () => {
  it("no file outside web/src/terminal/ statically imports @xterm/*", () => {
    const offenders: string[] = [];
    for (const file of walk(SRC)) {
      if (file.includes("/terminal/")) continue;
      const text = nodeFs.readFileSync(file, "utf8");
      for (const spec of findXtermImports(text)) offenders.push(`${file}: ${spec}`);
    }
    expect(offenders).toEqual([]);
  });

  it("TerminalSlot.tsx contains lazy( and no @xterm import", () => {
    const slot = nodeFs.readFileSync(here("../components/TerminalSlot.tsx"), "utf8");
    expect(slot).toContain("lazy(");
    expect(findXtermImports(slot)).toEqual([]);
  });

  it("within terminal/, only TerminalPanel.lazy.ts references ./TerminalPanel, and never wraps it in another lazy(", () => {
    const dir = here("../../src/terminal/");
    const files = walk(dir).filter((f) => !f.endsWith(".test.ts") && !f.endsWith(".test.tsx"));
    const refs: string[] = [];
    for (const file of files) {
      const text = nodeFs.readFileSync(file, "utf8");
      if (text.includes('./TerminalPanel"') || text.includes("./TerminalPanel'")) {
        refs.push(file);
      }
    }
    expect(refs).toEqual([dir + "TerminalPanel.lazy.ts"]);
    const lazyText = nodeFs.readFileSync(dir + "TerminalPanel.lazy.ts", "utf8");
    // A plain re-export into the async chunk. Wrapping ./TerminalPanel in
    // a second React.lazy makes the slot's lazy() resolve to a lazy object
    // and React throws error #306 (e2e: the panel never mounted).
    expect(lazyText).toMatch(/export\s+\{[^}]*\}\s+from\s+"\.\/TerminalPanel"/);
    expect(lazyText).not.toContain("lazy(");
  });
});
