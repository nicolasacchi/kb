// OL F10 — the nav-count gate. ui/nav-budget.json holds the number of doors
// this app's top bar may show; equality so a lower count lowers the budget in
// the same PR.
import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";
import { describe, expect, it } from "vitest";
import { NAV_ITEMS } from "./navItems";

const budget = JSON.parse(
  readFileSync(fileURLToPath(new URL("../../../ui/nav-budget.json", import.meta.url)), "utf8"),
) as Record<string, number>;

describe("kb-code nav-item budget (ui/nav-budget.json)", () => {
  it("has exactly the budgeted number of doors", () => {
    expect(NAV_ITEMS.length, "lower ui/nav-budget.json `kb-code` in the same PR that removes a door").toBe(
      budget["kb-code"],
    );
  });
});
