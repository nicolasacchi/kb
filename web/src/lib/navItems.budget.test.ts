// OL F10 — the nav-count gate. ui/nav-budget.json holds the number of primary
// navigation items this app may show; the L/C units lower it in the PR that
// removes an item. Equality, not <=: a lower count must lower the budget too.
import { describe, expect, it } from "vitest";
import { NAV_ITEMS } from "./navItems";

// Node's fs via `process.getBuiltinModule` typed as `any` (web/ has no Node
// typings; same trick as styles/tokens.palette.test.ts).
// eslint-disable-next-line @typescript-eslint/no-explicit-any
const fs = (globalThis as any).process.getBuiltinModule("node:fs") as {
  readFileSync(url: URL, enc: "utf8"): string;
};
const budget = JSON.parse(
  fs.readFileSync(new URL("../../../ui/nav-budget.json", import.meta.url), "utf8"),
) as Record<string, number>;

describe("kb nav-item budget (ui/nav-budget.json)", () => {
  it("has exactly the budgeted number of primary tabs", () => {
    expect(NAV_ITEMS.length, "lower ui/nav-budget.json `kb` in the same PR that removes a tab").toBe(budget.kb);
  });

  it("has no duplicate ids", () => {
    expect(new Set(NAV_ITEMS.map((i) => i.id)).size).toBe(NAV_ITEMS.length);
  });
});
