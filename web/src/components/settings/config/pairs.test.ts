import { describe, expect, it } from "vitest";
import { pairsToRecord, recordToPairs, type Pair } from "./fields";

// QFIX-7 — pins the record round-trip that made "+ add" a no-op when
// empty-keyed rows were committed immediately, and documents last-wins
// for duplicate keys (draft UI keeps both rows; the record collapses).

describe("pairsToRecord", () => {
  it("drops empty keys", () => {
    expect(pairsToRecord([{ a: "", b: "x" }, { a: "  ", b: "y" }])).toEqual({});
  });
  it("keeps non-empty keys including empty values", () => {
    expect(pairsToRecord([{ a: "t", b: "" }])).toEqual({ t: "" });
  });
  it("last-wins on duplicate keys", () => {
    const pairs: Pair[] = [
      { a: "k", b: "first" },
      { a: "k", b: "second" },
    ];
    expect(pairsToRecord(pairs)).toEqual({ k: "second" });
  });
  it("preserves multiple distinct keys", () => {
    expect(
      pairsToRecord([
        { a: "a", b: "1" },
        { a: "b", b: "2" },
      ]),
    ).toEqual({ a: "1", b: "2" });
  });
});

describe("recordToPairs / round-trip", () => {
  it("round-trips a record without empties or dups", () => {
    const rec = { foo: "/a.html", bar: "/b.html" };
    expect(pairsToRecord(recordToPairs(rec))).toEqual(rec);
  });
  it("empty-keyed draft rows vanish on commit (parent store shape)", () => {
    // The local draft in PairListField keeps these; pairsToRecord is what
    // the templates field writes into config.
    const draft: Pair[] = [
      { a: "keep", b: "/k.html" },
      { a: "", b: "" },
    ];
    expect(pairsToRecord(draft)).toEqual({ keep: "/k.html" });
    expect(recordToPairs(pairsToRecord(draft))).toEqual([{ a: "keep", b: "/k.html" }]);
  });
});
