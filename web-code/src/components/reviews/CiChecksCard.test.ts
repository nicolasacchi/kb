import { describe, expect, it } from "vitest";
import type { CheckRunOut } from "../../api/types";
import { checksAbsenceReason, formatFetchedCaption, summarizeChecks } from "./CiChecksCard";

function check(overrides: Partial<CheckRunOut> = {}): CheckRunOut {
  return { name: "specs", status: "pass", ...overrides };
}

describe("summarizeChecks", () => {
  it("summarizes an empty list as all-zero with no worst status", () => {
    expect(summarizeChecks([])).toEqual({ total: 0, pass: 0, fail: 0, warn: 0, pending: 0, worst: null });
  });

  it("counts each status bucket", () => {
    const s = summarizeChecks([
      check({ status: "pass" }),
      check({ status: "pass" }),
      check({ status: "fail" }),
      check({ status: "warn" }),
      check({ status: "pending" }),
    ]);
    expect(s).toMatchObject({ total: 5, pass: 2, fail: 1, warn: 1, pending: 1 });
  });

  it("worst status is fail when any check failed, even with passes present", () => {
    const s = summarizeChecks([check({ status: "pass" }), check({ status: "fail" })]);
    expect(s.worst).toBe("fail");
  });

  it("worst status is pending over warn when both present and no fail", () => {
    const s = summarizeChecks([check({ status: "warn" }), check({ status: "pending" })]);
    expect(s.worst).toBe("pending");
  });

  it("worst status is pass when every check passed", () => {
    const s = summarizeChecks([check({ status: "pass" }), check({ status: "pass" })]);
    expect(s.worst).toBe("pass");
  });
});

describe("checksAbsenceReason", () => {
  it("names the GitHub-unavailable reason when one is present, distinct from a genuine empty list", () => {
    expect(checksAbsenceReason("rate-limited", 0)).toBe(
      "checks not fetched — GitHub unavailable (rate-limited)",
    );
  });

  it("names a genuinely empty check list distinctly, with no unavailable_reason", () => {
    expect(checksAbsenceReason(undefined, 0)).toBe("no CI checks reported for this PR's head commit");
  });

  it("returns null (no absence) once at least one check fetched successfully", () => {
    expect(checksAbsenceReason(undefined, 3)).toBeNull();
  });

  it("prefers the unavailable_reason even if total happens to be nonzero (a stale prior fetch)", () => {
    expect(checksAbsenceReason("timeout", 2)).toContain("timeout");
  });
});

describe("formatFetchedCaption", () => {
  it("is null when no fetch has happened yet", () => {
    expect(formatFetchedCaption(undefined)).toBeNull();
  });

  it("formats a unix-seconds timestamp as zero-padded HH:MM", () => {
    // 2026-01-01T05:03:00Z-ish — just assert shape, not exact TZ-dependent value.
    const caption = formatFetchedCaption(1735707780);
    expect(caption).toMatch(/^fetched \d{2}:\d{2}$/);
  });
});
