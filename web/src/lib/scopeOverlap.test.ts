import { describe, expect, it } from "vitest";
import {
  aggregateScopeOverlap,
  scopeOverlapColumns,
  scopeOverlapPivotKb,
  type ScopeOverlapInput,
} from "./scopeOverlap";

function hit(kb: string, global: boolean, linked: string[] = []): ScopeOverlapInput {
  return { kb, global, linked_kbs: linked };
}

describe("aggregateScopeOverlap", () => {
  it("is empty for no memories", () => {
    expect(aggregateScopeOverlap([])).toEqual([]);
  });

  it("groups private (unlinked) memories per home kb", () => {
    const rows = aggregateScopeOverlap([hit("alpha", false), hit("alpha", false), hit("beta", false)]);
    expect(rows).toEqual([
      { key: "alpha", members: ["alpha"], isGlobal: false, count: 2 },
      { key: "beta", members: ["beta"], isGlobal: false, count: 1 },
    ]);
  });

  it("groups a combo by its MEMBER SET regardless of which side is 'home'", () => {
    const rows = aggregateScopeOverlap([
      hit("alpha", false, ["beta"]),
      hit("beta", false, ["alpha"]),
    ]);
    expect(rows).toEqual([{ key: "alpha+beta", members: ["alpha", "beta"], isGlobal: false, count: 2 }]);
  });

  it("dedups a link set with repeats/self-references", () => {
    const rows = aggregateScopeOverlap([hit("alpha", false, ["alpha", "beta", "beta"])]);
    expect(rows).toEqual([{ key: "alpha+beta", members: ["alpha", "beta"], isGlobal: false, count: 1 }]);
  });

  it("groups every global memory under ONE row, tracking home kbs as members", () => {
    const rows = aggregateScopeOverlap([hit("alpha", true), hit("beta", true), hit("alpha", true)]);
    expect(rows).toEqual([{ key: "*", members: ["alpha", "beta"], isGlobal: true, count: 3 }]);
  });

  it("sorts the global row first regardless of count", () => {
    const rows = aggregateScopeOverlap([
      hit("alpha", false),
      hit("alpha", false),
      hit("alpha", false),
      hit("beta", true),
    ]);
    expect(rows[0].isGlobal).toBe(true);
    expect(rows[0].count).toBe(1);
  });

  it("sorts non-global rows by count desc, then key asc on ties", () => {
    const rows = aggregateScopeOverlap([
      hit("z", false),
      hit("a", false),
      hit("b", false),
      hit("b", false),
    ]);
    expect(rows.map((r) => r.key)).toEqual(["b", "a", "z"]);
  });
});

describe("scopeOverlapColumns", () => {
  it("is empty for no rows", () => {
    expect(scopeOverlapColumns([])).toEqual([]);
  });

  it("unions and sorts every row's members", () => {
    const rows = aggregateScopeOverlap([
      hit("beta", false),
      hit("alpha", false, ["gamma"]),
    ]);
    expect(scopeOverlapColumns(rows)).toEqual(["alpha", "beta", "gamma"]);
  });
});

describe("scopeOverlapPivotKb", () => {
  it("picks the alphabetically-first member", () => {
    const rows = aggregateScopeOverlap([hit("beta", false, ["alpha"])]);
    expect(scopeOverlapPivotKb(rows[0])).toBe("alpha");
  });

  it("returns null for a defensive empty-members row", () => {
    expect(scopeOverlapPivotKb({ key: "", members: [], isGlobal: false, count: 0 })).toBeNull();
  });
});
