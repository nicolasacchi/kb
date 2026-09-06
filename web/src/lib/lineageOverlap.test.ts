import { describe, expect, it } from "vitest";
import { computeOverlapWindow, ongoingOverlapDays } from "./lineageOverlap";

const NOW = 1_700_000_000;
const DAY = 86_400;

describe("computeOverlapWindow", () => {
  it("returns null when the newer node has no creation time", () => {
    expect(
      computeOverlapWindow({ createdUnix: NOW, forgotten: false }, { createdUnix: null, forgotten: false }),
    ).toBeNull();
  });

  it("is ongoing when the older fact is still not forgotten", () => {
    const w = computeOverlapWindow(
      { createdUnix: NOW - 10 * DAY, forgotten: false },
      { createdUnix: NOW - 5 * DAY, forgotten: false },
    );
    expect(w).toEqual({ startUnix: NOW - 5 * DAY, ongoing: true });
  });

  it("is resolved (not ongoing) once the older fact is forgotten", () => {
    const w = computeOverlapWindow(
      { createdUnix: NOW - 10 * DAY, forgotten: true },
      { createdUnix: NOW - 5 * DAY, forgotten: false },
    );
    expect(w).toEqual({ startUnix: NOW - 5 * DAY, ongoing: false });
  });
});

describe("ongoingOverlapDays", () => {
  it("computes days elapsed for an ongoing window", () => {
    const w = { startUnix: NOW - 10 * DAY, ongoing: true };
    expect(ongoingOverlapDays(w, NOW)).toBeCloseTo(10, 5);
  });

  it("returns null for a resolved window — no fabricated end date", () => {
    const w = { startUnix: NOW - 10 * DAY, ongoing: false };
    expect(ongoingOverlapDays(w, NOW)).toBeNull();
  });

  it("clamps to zero rather than going negative for a future start (clock skew)", () => {
    const w = { startUnix: NOW + DAY, ongoing: true };
    expect(ongoingOverlapDays(w, NOW)).toBe(0);
  });
});
