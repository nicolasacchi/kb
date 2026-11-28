import { describe, expect, it } from "vitest";
import { groupSwitcher, stepTerm, flattenSwitcher } from "./switchModel";
import { fixtureTerms, mkTerm, ordered } from "./switchTestKit";

const ids = (rows: { term: { id: string } }[]): string[] => rows.map((r) => r.term.id);

describe("groupSwitcher", () => {
  const rows = ordered(fixtureTerms());

  it("splits by the open terminal's project, keeps orderHub order, needs leads", () => {
    // order: e(needs 50) a(needs 100) b(working 300) c(working 200) d(ended)
    expect(ids(rows)).toEqual(["e", "a", "b", "c", "d"]);
    const g = groupSwitcher(rows, "p1");
    expect(ids(g.thisProject)).toEqual(["a", "b"]);
    expect(ids(g.everywhere)).toEqual(["e", "c"]);
  });

  it("puts ended rows in `ended` only, whatever their project", () => {
    const g = groupSwitcher(rows, "p2");
    expect(ids(g.ended)).toEqual(["d"]);
    expect(ids([...g.thisProject, ...g.everywhere])).not.toContain("d");
    expect(ids(g.thisProject)).toEqual(["e", "c"]);
  });

  it("a null project yields no This project group", () => {
    const g = groupSwitcher(rows, null);
    expect(g.thisProject).toEqual([]);
    expect(ids(g.everywhere)).toEqual(["e", "a", "b", "c"]);
  });

  it("moves needs rows ahead even when the input is not tier-ordered", () => {
    const shuffled = [rows[2], rows[0], rows[3], rows[1], rows[4]]; // b e c a d
    const g = groupSwitcher(shuffled, "p1");
    expect(ids(g.thisProject)).toEqual(["a", "b"]);
    expect(ids(g.everywhere)).toEqual(["e", "c"]);
  });

  it("does not mutate its input", () => {
    const copy = [...rows];
    groupSwitcher(rows, "p1");
    expect(rows).toEqual(copy);
  });
});

describe("stepTerm live", () => {
  const rows = ordered(fixtureTerms());
  // from b (p1): display order is a b | e c
  it("flattens this project then everywhere", () => {
    expect(ids(flattenSwitcher(rows, "b"))).toEqual(["a", "b", "e", "c"]);
  });
  it("steps forward and wraps", () => {
    expect(stepTerm(rows, "b", 1, "live")).toBe("e");
    expect(stepTerm(rows, "c", 1, "live")).toBe("a");
  });
  it("steps backward and wraps", () => {
    expect(stepTerm(rows, "b", -1, "live")).toBe("a");
    expect(stepTerm(rows, "a", -1, "live")).toBe("c");
  });
  it("never returns an ended row, and an ended current starts at an end", () => {
    for (const from of ["a", "b", "c", "e"]) {
      expect(stepTerm(rows, from, 1, "live")).not.toBe("d");
      expect(stepTerm(rows, from, -1, "live")).not.toBe("d");
    }
    expect(stepTerm(rows, "d", 1, "live")).not.toBe("d");
    expect(stepTerm(rows, "d", 1, "live")).not.toBeNull();
  });
  it("an unknown or null current starts at the first (next) or last (previous)", () => {
    expect(stepTerm(rows, null, 1, "live")).toBe(flattenSwitcher(rows, null)[0].term.id);
    expect(stepTerm(rows, "zz", -1, "live")).toBe("c");
  });
  it("is null for no rows and for a lone current row", () => {
    expect(stepTerm([], null, 1, "live")).toBeNull();
    expect(stepTerm(ordered([mkTerm("x")]), "x", 1, "live")).toBeNull();
    expect(stepTerm(ordered([mkTerm("x")]), "x", -1, "live")).toBeNull();
    expect(stepTerm(ordered([mkTerm("x"), mkTerm("y", { pane_dead: true })]), "x", 1, "live")).toBeNull();
  });
  it("is stable when the same rows arrive shuffled and are re-ordered", () => {
    const t = fixtureTerms();
    const again = ordered([t[3], t[1], t[4], t[0], t[2]]);
    expect(stepTerm(again, "b", 1, "live")).toBe(stepTerm(rows, "b", 1, "live"));
    expect(stepTerm(again, "a", -1, "live")).toBe(stepTerm(rows, "a", -1, "live"));
  });
});

describe("stepTerm needs", () => {
  const rows = ordered(fixtureTerms()); // needs order: e (50), a (100)
  it("from a row that does not need you jumps to the oldest needs row", () => {
    expect(stepTerm(rows, "b", 1, "needs")).toBe("e");
    expect(stepTerm(rows, "c", -1, "needs")).toBe("e");
    expect(stepTerm(rows, null, 1, "needs")).toBe("e");
  });
  it("between needs rows steps oldest first and wraps both ways", () => {
    expect(stepTerm(rows, "e", 1, "needs")).toBe("a");
    expect(stepTerm(rows, "a", 1, "needs")).toBe("e");
    expect(stepTerm(rows, "e", -1, "needs")).toBe("a");
  });
  it("is null with no needs rows and with only the current needs row", () => {
    expect(stepTerm(ordered([mkTerm("x"), mkTerm("y")]), "x", 1, "needs")).toBeNull();
    expect(stepTerm(ordered([mkTerm("x", { blocked: true }), mkTerm("y")]), "x", 1, "needs")).toBeNull();
    expect(stepTerm(ordered([mkTerm("x", { blocked: true }), mkTerm("y")]), "y", 1, "needs")).toBe("x");
  });
});
