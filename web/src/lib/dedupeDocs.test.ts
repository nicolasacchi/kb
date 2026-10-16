import { describe, expect, it } from "vitest";
import { dedupeDocRows, docKey } from "./dedupeDocs";

describe("dedupeDocRows", () => {
  it("keeps one row per id, first occurrence, order preserved", () => {
    const rows = [
      { id: "a", t: 1 },
      { id: "b", t: 2 },
      { id: "a", t: 3 },
      { id: "c", t: 4 },
      { id: "b", t: 5 },
    ];
    expect(dedupeDocRows("canon", rows)).toEqual([
      { id: "a", t: 1 },
      { id: "b", t: 2 },
      { id: "c", t: 4 },
    ]);
  });

  it("is a no-op on a clean list and does not mutate its input", () => {
    const rows = [{ id: "a" }, { id: "b" }];
    const out = dedupeDocRows("canon", rows);
    expect(out).toEqual(rows);
    expect(rows).toHaveLength(2);
  });

  it("keys on the (kb, id) pair", () => {
    expect(docKey("x", "1")).not.toBe(docKey("y", "1"));
    expect(docKey(null, "1")).toBe(":1");
  });
});
