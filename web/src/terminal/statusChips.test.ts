import { describe, expect, it } from "vitest";
import { statusChips } from "./statusChips";

describe("statusChips", () => {
  it("maps every field to a chip in the fixed order", () => {
    const chips = statusChips({
      branch: "main",
      permission_mode: "plan",
      cost_usd: 1.5,
      context_pct: 41.6,
      model: "opus",
    });
    expect(chips.map((c) => [c.id, c.label])).toEqual([
      ["model", "opus"],
      ["context", "ctx 42%"],
      ["cost", "$1.50"],
      ["mode", "plan"],
      ["branch", "main"],
    ]);
  });

  it("absent or undefined status yields no chips", () => {
    expect(statusChips(undefined)).toEqual([]);
    expect(statusChips(null)).toEqual([]);
    expect(statusChips({})).toEqual([]);
  });

  it("omits the cost chip unless the cost is reported (zero is reported)", () => {
    expect(statusChips({ model: "m" }).some((c) => c.id === "cost")).toBe(false);
    expect(statusChips({ cost_usd: 0 }).map((c) => c.label)).toEqual(["$0.00"]);
  });

  it("omits default and other permission modes, labels the three that matter", () => {
    expect(statusChips({ permission_mode: "default" })).toEqual([]);
    expect(statusChips({ permission_mode: "other" })).toEqual([]);
    expect(statusChips({ permission_mode: "accept" })[0]?.label).toBe("accept");
    expect(statusChips({ permission_mode: "bypass" })[0]?.label).toBe("bypass");
  });

  it("every chip title names its source and its freshness", () => {
    for (const c of statusChips({ model: "m", context_pct: 1, cost_usd: 1, permission_mode: "plan", branch: "b" })) {
      expect(c.title).toContain("reported by the harness hook");
      expect(c.title).toContain("polled every 10 s");
    }
  });

  it("keeps markup in a model name as plain text", () => {
    const [chip] = statusChips({ model: "<b>x</b>" });
    expect(chip?.label).toBe("<b>x</b>");
  });
});
