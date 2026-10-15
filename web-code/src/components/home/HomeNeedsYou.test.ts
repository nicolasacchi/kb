import { describe, expect, it } from "vitest";
import { nonEmptyLanes } from "./HomeNeedsYou";

describe("nonEmptyLanes (OL-C7)", () => {
  it("is empty when every lane is zero or absent", () => {
    expect(nonEmptyLanes(0, 0, null, null)).toEqual([]);
    expect(nonEmptyLanes(0, 0, 0, 0)).toEqual([]);
  });

  it("keeps only lanes with a count, in display order, with singular labels", () => {
    const rows = nonEmptyLanes(1, 0, 3, null);
    expect(rows.map((r) => r.lane)).toEqual(["reviews", "kb-desk"]);
    expect(rows[0].label).toBe("1 review awaiting you");
    expect(rows[1].label).toBe("3 on your kb desk");
  });
});
