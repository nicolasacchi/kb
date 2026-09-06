import { describe, expect, it } from "vitest";
import type { ImpactAnalysisOut } from "../api/types";
import {
  bucketCount,
  flattenImpact,
  impactReducer,
  initialImpactState,
  isImpactNavigable,
  provenanceFor,
} from "./impactState";

function sample(): ImpactAnalysisOut {
  return {
    schema: "impact/1",
    symbol: { name: "foo", kind: "function", path: "a.rs", line: 1 },
    direct_exact: [
      { path: "b.rs", line: 10, col: 0, class: "exact", kind: "usage", name: "bar" },
    ],
    direct_likely: [
      { path: "c.rs", line: 20, col: 0, class: "likely", kind: "caller", name: "baz" },
    ],
    transitive: [
      { path: "d.rs", line: 30, col: 0, class: "likely", kind: "transitive", depth: 2, name: "t2" },
      { path: "e.rs", line: 40, col: 0, class: "candidate", kind: "transitive", depth: 1, name: "t1" },
    ],
    imports: [{ path: "f.rs", line: 50, col: 0, class: "exact", kind: "import" }],
    tests: [{ path: "tests/g.rs", line: 60, col: 0, class: "exact", kind: "usage", name: "test_foo" }],
    truncated: {
      direct_exact: false,
      direct_likely: false,
      transitive: true,
      imports: false,
      tests: false,
    },
    provenance: {
      direct_exact: {
        rows_with_session: 1,
        distinct_sessions: 1,
        sample: [{ session_id: "sid-aaaa-bbbb-cccc-dddddddddddd", path: "b.rs", line: 10 }],
      },
    },
    note: "compositional impact over name resolution — honesty contract",
  };
}

describe("flattenImpact", () => {
  it("emits buckets in required order with Tests last", () => {
    const flat = flattenImpact(sample(), new Set());
    const sections = flat.filter((r) => r.section).map((r) => r.section);
    expect(sections).toEqual([
      "direct_exact",
      "direct_likely",
      "transitive",
      "imports",
      "tests",
    ]);
  });

  it("groups transitive by depth ascending", () => {
    const flat = flattenImpact(sample(), new Set());
    const depthGroups = flat.filter((r) => r.depthGroup != null).map((r) => r.depthGroup);
    expect(depthGroups).toEqual([1, 2]);
  });

  it("includes truncation note when truncated", () => {
    const flat = flattenImpact(sample(), new Set());
    expect(flat.some((r) => r.truncatedNote)).toBe(true);
  });

  it("hides body rows when a bucket is collapsed", () => {
    const flat = flattenImpact(sample(), new Set(["tests"]));
    expect(flat.some((r) => r.path === "tests/g.rs")).toBe(false);
    expect(flat.some((r) => r.section === "tests")).toBe(true);
  });
});

describe("impactReducer", () => {
  it("OPEN → loading; SET_DATA → flat + note", () => {
    let s = impactReducer(initialImpactState, { type: "OPEN", title: "foo" });
    expect(s.open).toBe(true);
    expect(s.loading).toBe(true);
    s = impactReducer(s, { type: "SET_DATA", data: sample() });
    expect(s.loading).toBe(false);
    expect(s.note).toContain("compositional impact");
    expect(s.flat.length).toBeGreaterThan(5);
  });

  it("MOVE clamps cursor", () => {
    let s = impactReducer(initialImpactState, { type: "OPEN", title: "foo" });
    s = impactReducer(s, { type: "SET_DATA", data: sample() });
    s = impactReducer(s, { type: "MOVE", delta: 1000 });
    expect(s.cursor).toBe(s.flat.length - 1);
  });
});

describe("helpers", () => {
  it("bucketCount + provenanceFor", () => {
    const d = sample();
    expect(bucketCount(d, "tests")).toBe(1);
    expect(provenanceFor(d, "direct_exact")?.distinct_sessions).toBe(1);
    expect(provenanceFor(d, "tests")).toBeNull();
  });

  it("isImpactNavigable", () => {
    expect(
      isImpactNavigable({
        id: "1",
        path: "a.rs",
        line: 1,
        col: 0,
        class: "exact",
        kind: "usage",
      }),
    ).toBe(true);
    expect(
      isImpactNavigable({
        id: "2",
        section: "tests",
        path: "",
        line: 0,
        col: 0,
        class: "exact",
        kind: "section",
      }),
    ).toBe(false);
  });
});
