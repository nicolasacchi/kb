import { describe, expect, it } from "vitest";

// Node's fs via `process.getBuiltinModule` typed loosely (web/ has no Node
// typings; same trick as lib/navItems.budget.test.ts).
// eslint-disable-next-line @typescript-eslint/no-explicit-any
const fs = (globalThis as any).process.getBuiltinModule("node:fs") as {
  readFileSync(url: URL, enc: "utf8"): string;
};

// OL P4 — Lists and Settings share ONE page frame (`.kb-page`): same
// max-width, centring and top padding. Source-level pin: the frame rule
// exists, and both pages' root elements use it.
const read = (rel: string) =>
  fs.readFileSync(new URL(rel, import.meta.url), "utf8");

describe("page frame", () => {
  const css = read("../styles/app.css");
  const rule = /\.kb-page\s*\{([^}]*)\}/.exec(css)?.[1] ?? "";

  it(".kb-page caps the width, centres, and pads the top", () => {
    expect(rule).toMatch(/max-width:\s*1100px/);
    expect(rule).toMatch(/margin-left:\s*auto/);
    expect(rule).toMatch(/margin-right:\s*auto/);
    expect(rule).toMatch(/padding-top:/);
  });

  it("Settings and Lists roots both use it", () => {
    expect(read("./settings.tsx")).toMatch(/className="kb-page settings"/);
    expect(read("./lists.tsx")).toMatch(/className="kb-page kb-lists"/);
  });

  it("Lists no longer carries its own left-aligned 980px cap", () => {
    expect(read("../styles/lists.css")).not.toMatch(/max-width:\s*980px/);
  });
});
