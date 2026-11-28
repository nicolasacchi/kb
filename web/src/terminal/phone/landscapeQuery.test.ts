// FUMINPHONE (POLISH2 follow-up) - one landscape-phone query. CSS cannot import a
// constant, so the stylesheets restate it; this fails when they diverge.
import { describe, expect, it } from "vitest";
import { LANDSCAPE_PHONE_QUERY } from "./queries";

// Node's fs without Node typings (same trick as lib/codeDoors.runtime.test.ts).
// eslint-disable-next-line @typescript-eslint/no-explicit-any
const fs = (globalThis as any).process.getBuiltinModule("node:fs") as {
  readFileSync(path: string, enc: "utf8"): string;
};
const read = (rel: string): string => fs.readFileSync(decodeURIComponent(new URL(rel, import.meta.url).pathname), "utf8");

/** Every `@media (...)` condition in the sheet that talks about orientation. */
function landscapeQueries(css: string): string[] {
  return [...css.matchAll(/@media\s+([^{]+)\{/g)].map((m) => m[1].trim()).filter((q) => q.includes("orientation"));
}

describe("the landscape-phone media query", () => {
  it("has the intended shape", () => {
    expect(LANDSCAPE_PHONE_QUERY).toBe("(pointer: coarse) and (orientation: landscape) and (max-height: 599px)");
  });

  for (const file of ["./drive.css", "./phone.css", "../commands/palette/palette.css"]) {
    it(`${file} uses exactly the constant wherever it asks about orientation`, () => {
      const qs = landscapeQueries(read(file));
      expect(qs.length, `${file} has no landscape query`).toBeGreaterThan(0);
      for (const q of qs) expect(q).toBe(LANDSCAPE_PHONE_QUERY);
    });
  }
});
