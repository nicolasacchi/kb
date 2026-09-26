// RS-U11 — parseLastMaint/parseLastGcDryRun/parseLastGcApply over
// state_json, defensively: every malformed/absent shape degrades to
// `null`, never a crash or a guessed value.
import { describe, expect, it } from "vitest";
import { parseLastGcApply, parseLastGcDryRun, parseLastMaint } from "./reviewStore";

describe("parseLastMaint", () => {
  it("reads all three cadences when present", () => {
    expect(parseLastMaint({ last_maint: { daily: 100, weekly: 200, monthly: 300 } })).toEqual({
      daily: 100,
      weekly: 200,
      monthly: 300,
    });
  });

  it("a cadence that has never run reads null, the others still read", () => {
    expect(parseLastMaint({ last_maint: { daily: 100, weekly: null, monthly: null } })).toEqual({
      daily: 100,
      weekly: null,
      monthly: null,
    });
  });

  it("degrades to null when no cadence has EVER run (all three absent/null)", () => {
    expect(parseLastMaint({ last_maint: { daily: null, weekly: null, monthly: null } })).toBeNull();
    expect(parseLastMaint({ last_maint: {} })).toBeNull();
  });

  it("degrades to null on a missing key, a non-object state_json, or a malformed shape", () => {
    expect(parseLastMaint({})).toBeNull();
    expect(parseLastMaint(null)).toBeNull();
    expect(parseLastMaint(undefined)).toBeNull();
    expect(parseLastMaint("not an object")).toBeNull();
    expect(parseLastMaint({ last_maint: "not an object either" })).toBeNull();
    expect(parseLastMaint({ last_maint: [1, 2, 3] })).toBeNull();
  });
});

describe("parseLastGcDryRun", () => {
  it("reads the full shape, including the plural reasons array", () => {
    expect(
      parseLastGcDryRun({
        last_gc_dry_run: {
          at: 1000,
          candidates: 3,
          reasons: ["dry-run"],
          partial: false,
          member_problems: [],
        },
      }),
    ).toEqual({ at: 1000, candidates: 3, reasons: ["dry-run"], partial: false, member_problems: [] });
  });

  it("surfaces partial + member_problems honestly rather than dropping them", () => {
    const parsed = parseLastGcDryRun({
      last_gc_dry_run: {
        at: 1000,
        candidates: 0,
        reasons: ["restore-guard"],
        partial: true,
        member_problems: ["member 7: common_dir_of failed"],
      },
    });
    expect(parsed?.partial).toBe(true);
    expect(parsed?.member_problems).toEqual(["member 7: common_dir_of failed"]);
  });

  it("degrades to null with no `at` (the one required field) or a malformed shape", () => {
    expect(parseLastGcDryRun({})).toBeNull();
    expect(parseLastGcDryRun({ last_gc_dry_run: { candidates: 3 } })).toBeNull();
    expect(parseLastGcDryRun(null)).toBeNull();
    expect(parseLastGcDryRun({ last_gc_dry_run: "nope" })).toBeNull();
  });

  it("a non-array reasons/member_problems degrades to an empty array, not a crash", () => {
    expect(
      parseLastGcDryRun({ last_gc_dry_run: { at: 1, candidates: 1, reasons: "oops", member_problems: 5 } }),
    ).toEqual({ at: 1, candidates: 1, reasons: [], partial: false, member_problems: [] });
  });
});

describe("parseLastGcApply", () => {
  it("reads the shape — a SINGULAR reason, unlike the dry-run's reasons[]", () => {
    expect(parseLastGcApply({ last_gc_apply: { at: 2000, candidates: 5, reason: "applied" } })).toEqual({
      at: 2000,
      candidates: 5,
      reason: "applied",
    });
  });

  it("degrades to null with no `at` or a malformed shape", () => {
    expect(parseLastGcApply({})).toBeNull();
    expect(parseLastGcApply({ last_gc_apply: { candidates: 5 } })).toBeNull();
    expect(parseLastGcApply(undefined)).toBeNull();
  });
});
