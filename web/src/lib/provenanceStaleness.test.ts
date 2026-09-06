import { describe, expect, it } from "vitest";
import { daysAgo, daysAgoLabel, fileStalenessLabel } from "./provenanceStaleness";

const DAY = 86400;

describe("daysAgo", () => {
  it("is 0 for the same instant", () => {
    expect(daysAgo(1000, 1000)).toBe(0);
  });

  it("floors partial days", () => {
    expect(daysAgo(1000, 1000 + DAY * 2.9)).toBe(2);
  });

  it("clamps negative (clock skew / future timestamp) to 0, never negative", () => {
    expect(daysAgo(2000, 1000)).toBe(0);
  });
});

describe("daysAgoLabel", () => {
  it("says 'today' for 0 days", () => {
    expect(daysAgoLabel(1000, 1000)).toBe("today");
  });

  it("says '1d ago' for exactly one day (singular, no 's')", () => {
    expect(daysAgoLabel(1000, 1000 + DAY)).toBe("1d ago");
  });

  it("says 'Nd ago' for multiple days", () => {
    expect(daysAgoLabel(1000, 1000 + DAY * 12)).toBe("12d ago");
  });
});

describe("fileStalenessLabel", () => {
  const now = 1_700_000_000;

  it("reports 'unknown' when the git lookup couldn't answer", () => {
    expect(fileStalenessLabel({ last_touched_unix: null, changed_since: null }, now)).toBe(
      "unknown — git lookup unavailable",
    );
  });

  it("reports 'unknown' when only one of the pair is present (defensive — should never happen)", () => {
    expect(fileStalenessLabel({ last_touched_unix: now, changed_since: null }, now)).toBe(
      "unknown — git lookup unavailable",
    );
    expect(fileStalenessLabel({ last_touched_unix: null, changed_since: false }, now)).toBe(
      "unknown — git lookup unavailable",
    );
  });

  it("reports 'changed again Nd ago' when changed_since is true", () => {
    expect(
      fileStalenessLabel({ last_touched_unix: now - DAY * 3, changed_since: true }, now),
    ).toBe("changed again 3d ago");
  });

  it("reports 'unchanged since this commit (Nd ago)' when changed_since is false", () => {
    expect(
      fileStalenessLabel({ last_touched_unix: now - DAY * 30, changed_since: false }, now),
    ).toBe("unchanged since this commit (30d ago)");
  });
});
