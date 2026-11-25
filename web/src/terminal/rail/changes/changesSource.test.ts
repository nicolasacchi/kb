// RAILCHANGES — source guards: the tab never re-derives the checks verdict,
// never owns a fetching timer and never imports xterm.
import { describe, expect, it } from "vitest";

// eslint-disable-next-line @typescript-eslint/no-explicit-any
const nodeFs = (globalThis as any).process.getBuiltinModule("node:fs") as {
  readFileSync(path: string, enc: "utf8"): string;
};
const here = (rel: string): string => decodeURIComponent(new URL(rel, import.meta.url).pathname);
const FILES = ["ChangesRail.tsx", "changesModel.ts", "ChecksLazy.tsx"];

describe("RAILCHANGES sources", () => {
  for (const name of FILES) {
    const text = nodeFs.readFileSync(here(`./${name}`), "utf8");
    it(`${name} does not re-derive a verdict`, () => {
      expect(text).not.toMatch(/verdict\s*(===|!==|==)/);
      expect(text).not.toMatch(/["'](verified|stale|failing|unconfirmed|unchecked|no_edits|not_extracted)["']/);
    });
    it(`${name} has no xterm import, no refetchInterval, no fetch call`, () => {
      expect(text).not.toMatch(/@xterm\//);
      expect(text).not.toMatch(/refetchInterval/);
      expect(text).not.toMatch(/\bfetch\(/);
    });
  }
  it("the only timer is a local re-render tick", () => {
    const text = nodeFs.readFileSync(here("./ChangesRail.tsx"), "utf8");
    expect(text.match(/setInterval\(/g)?.length).toBe(1);
    expect(text).toContain("setNow(Date.now())");
  });
  it("ChecksSection is reached only through a lazy import of the session page module", () => {
    const text = nodeFs.readFileSync(here("./ChecksLazy.tsx"), "utf8");
    expect(text).toContain('import("../../../routes/sessions")');
    expect(text).not.toMatch(/^import .*routes\/sessions/m);
  });
});
