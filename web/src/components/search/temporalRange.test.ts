import { describe, it, expect } from "vitest";
import {
  dateInputToFromUnix,
  dateInputToToUnix,
  formatReadWindow,
  presetRange,
  unixToDateInput,
} from "./temporalRange";

// TZ is pinned to UTC by the `npm test` script (`TZ=UTC vitest`), so
// "local midnight" below is UTC midnight — deterministic on a dev box and
// in CI alike (matches lib/time.test.ts's convention).

describe("dateInputToFromUnix / dateInputToToUnix", () => {
  it("returns local midnight / end-of-day bounds for a valid date", () => {
    expect(dateInputToFromUnix("2026-07-21")).toBe(
      Date.parse("2026-07-21T00:00:00Z") / 1000,
    );
    expect(dateInputToToUnix("2026-07-21")).toBe(
      Date.parse("2026-07-21T23:59:59Z") / 1000,
    );
  });

  it("rejects empty/malformed/out-of-range input", () => {
    expect(dateInputToFromUnix("")).toBeNull();
    expect(dateInputToFromUnix("not-a-date")).toBeNull();
    expect(dateInputToFromUnix("2026-02-30")).toBeNull();
    expect(dateInputToToUnix("2026-13-01")).toBeNull();
  });
});

describe("unixToDateInput", () => {
  it("round-trips a from-bound back to its date-input value", () => {
    const from = dateInputToFromUnix("2026-01-05");
    expect(unixToDateInput(from)).toBe("2026-01-05");
  });

  it("round-trips a to-bound back to the same calendar day", () => {
    const to = dateInputToToUnix("2026-01-05");
    expect(unixToDateInput(to)).toBe("2026-01-05");
  });

  it("collapses null/undefined/non-finite to the empty string", () => {
    expect(unixToDateInput(null)).toBe("");
    expect(unixToDateInput(undefined)).toBe("");
    expect(unixToDateInput(Number.NaN)).toBe("");
  });
});

describe("presetRange", () => {
  const now = new Date("2026-07-21T15:30:00Z");

  it("today = just today's calendar day", () => {
    const { from, to } = presetRange("today", now);
    expect(unixToDateInput(from)).toBe("2026-07-21");
    expect(unixToDateInput(to)).toBe("2026-07-21");
    expect(to - from).toBe(86399);
  });

  it("7d = the last 7 calendar days inclusive of today", () => {
    const { from, to } = presetRange("7d", now);
    expect(unixToDateInput(from)).toBe("2026-07-15");
    expect(unixToDateInput(to)).toBe("2026-07-21");
    expect(to - from).toBe(6 * 86400 + 86399);
  });

  it("30d = the last 30 calendar days inclusive of today", () => {
    const { from, to } = presetRange("30d", now);
    expect(unixToDateInput(from)).toBe("2026-06-22");
    expect(unixToDateInput(to)).toBe("2026-07-21");
  });
});

describe("formatReadWindow", () => {
  it("renders a single day as one date, a span as a dash range", () => {
    const day = dateInputToFromUnix("2026-07-21");
    const dayEnd = dateInputToToUnix("2026-07-21");
    expect(formatReadWindow(day, dayEnd)).toBe("read 2026-07-21");

    const start = dateInputToFromUnix("2026-07-01");
    const end = dateInputToToUnix("2026-07-21");
    expect(formatReadWindow(start, end)).toBe("read 2026-07-01 – 2026-07-21");
  });

  it("renders an open-ended bound as since/until", () => {
    const from = dateInputToFromUnix("2026-07-01");
    expect(formatReadWindow(from, null)).toBe("read since 2026-07-01");
    const to = dateInputToToUnix("2026-07-21");
    expect(formatReadWindow(null, to)).toBe("read until 2026-07-21");
  });

  it("returns empty when neither bound is set", () => {
    expect(formatReadWindow(null, null)).toBe("");
    expect(formatReadWindow(undefined, undefined)).toBe("");
  });
});
