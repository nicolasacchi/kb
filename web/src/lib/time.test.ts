import { describe, it, expect, vi, afterEach } from "vitest";
import {
  formatHistoryTime,
  dayBucket,
  dayHeading,
  humanizeDuration,
  relativeAge,
  compactDate,
} from "./time";

// TZ is pinned to UTC by the `npm test` script (`TZ=UTC vitest`), so the
// calendar math below (getHours / getDate / local-midnight parsing) is
// deterministic on a dev box and in CI alike.

afterEach(() => {
  vi.useRealTimers();
});

describe("formatHistoryTime", () => {
  it("hhmm-only renders zero-padded HH:MM", () => {
    const unix = Date.parse("2026-01-02T03:05:00Z") / 1000;
    expect(formatHistoryTime(unix, "hhmm-only")).toBe("03:05");
  });
  it("hhmm-only renders midnight as 00:00", () => {
    const unix = Date.parse("2026-01-02T00:00:00Z") / 1000;
    expect(formatHistoryTime(unix, "hhmm-only")).toBe("00:00");
  });
  it("smart mode returns HH:MM for a same-day event", () => {
    vi.useFakeTimers();
    vi.setSystemTime(new Date("2026-01-02T20:00:00Z"));
    const unix = Date.parse("2026-01-02T08:30:00Z") / 1000;
    expect(formatHistoryTime(unix, "smart")).toBe("08:30");
  });
  it("smart mode returns 'yest.' for the previous day", () => {
    vi.useFakeTimers();
    vi.setSystemTime(new Date("2026-01-02T20:00:00Z"));
    const unix = Date.parse("2026-01-01T08:30:00Z") / 1000;
    expect(formatHistoryTime(unix, "smart")).toBe("yest.");
  });
  it("smart mode returns MM-DD for an older event", () => {
    vi.useFakeTimers();
    vi.setSystemTime(new Date("2026-01-10T20:00:00Z"));
    const unix = Date.parse("2026-01-02T08:30:00Z") / 1000;
    expect(formatHistoryTime(unix, "smart")).toBe("01-02");
  });
});

describe("dayBucket", () => {
  it("formats YYYY-MM-DD", () => {
    expect(dayBucket(Date.parse("2026-03-07T12:00:00Z") / 1000)).toBe("2026-03-07");
  });
  it("zero-pads single-digit month and day", () => {
    expect(dayBucket(Date.parse("2026-05-09T12:00:00Z") / 1000)).toBe("2026-05-09");
  });
});

describe("dayHeading", () => {
  it("labels today and yesterday", () => {
    vi.useFakeTimers();
    vi.setSystemTime(new Date("2026-01-10T12:00:00Z"));
    expect(dayHeading("2026-01-10")).toBe("Today");
    expect(dayHeading("2026-01-09")).toBe("Yesterday");
  });
  it("renders an older bucket as a long-form date", () => {
    vi.useFakeTimers();
    vi.setSystemTime(new Date("2026-01-10T12:00:00Z"));
    const h = dayHeading("2025-12-25");
    // Locale-robust: `toLocaleDateString` renders the year in the runtime's
    // default-locale numerals (Latin on en-US, Eastern-Arabic on ar-EG), so
    // we assert STRUCTURE, not a literal "2025" — only that it reformatted
    // into a non-empty label distinct from Today/Yesterday and the raw key.
    expect(h).not.toBe("Today");
    expect(h).not.toBe("Yesterday");
    expect(h).not.toBe("2025-12-25");
    expect(h.length).toBeGreaterThan(0);
  });
});

describe("humanizeDuration", () => {
  it("collapses zero / sub-second spans to empty", () => {
    expect(humanizeDuration(0)).toBe("");
    expect(humanizeDuration(400)).toBe("");
  });
  it("renders seconds, minutes, and hours compactly", () => {
    expect(humanizeDuration(45_000)).toBe("45s");
    expect(humanizeDuration(5 * 60_000)).toBe("5m");
    expect(humanizeDuration(63 * 60_000)).toBe("1h 3m");
    expect(humanizeDuration(120 * 60_000)).toBe("2h");
  });
});

describe("relativeAge", () => {
  const now = 1_000_000_000_000; // fixed "now" in ms
  const agoSecs = (s: number) => Math.floor(now / 1000) - s;
  it("returns '' for null / undefined", () => {
    expect(relativeAge(null, now)).toBe("");
    expect(relativeAge(undefined, now)).toBe("");
  });
  it("collapses the last minute (and the future) to 'now'", () => {
    expect(relativeAge(agoSecs(0), now)).toBe("now");
    expect(relativeAge(agoSecs(3), now)).toBe("now");
    expect(relativeAge(agoSecs(30), now)).toBe("now");
    expect(relativeAge(Math.floor(now / 1000) + 100, now)).toBe("now");
  });
  it("buckets by magnitude with a single token (session-card thresholds)", () => {
    expect(relativeAge(agoSecs(90), now)).toBe("1m");
    expect(relativeAge(agoSecs(5 * 60), now)).toBe("5m");
    expect(relativeAge(agoSecs(3 * 3600), now)).toBe("3h");
    expect(relativeAge(agoSecs(2 * 86_400), now)).toBe("2d");
    // QDUP threshold table: 10d stays days; 20d / 40d enter weeks.
    expect(relativeAge(agoSecs(10 * 86_400), now)).toBe("10d");
    expect(relativeAge(agoSecs(20 * 86_400), now)).toBe("2w");
    expect(relativeAge(agoSecs(40 * 86_400), now)).toBe("5w");
    expect(relativeAge(agoSecs(70 * 86_400), now)).toBe("2mo");
    expect(relativeAge(agoSecs(400 * 86_400), now)).toBe("1y");
  });
  it("pins the exact bucket boundaries (guards < vs <= off-by-ones)", () => {
    expect(relativeAge(agoSecs(60), now)).toBe("1m");
    expect(relativeAge(agoSecs(3600), now)).toBe("1h");
    expect(relativeAge(agoSecs(86400), now)).toBe("1d");
    expect(relativeAge(agoSecs(14 * 86400), now)).toBe("2w");
    expect(relativeAge(agoSecs(60 * 86400), now)).toBe("2mo");
    expect(relativeAge(agoSecs(365 * 86400), now)).toBe("1y");
  });
});

describe("compactDate", () => {
  it("renders Mon D for same calendar year", () => {
    vi.useFakeTimers();
    vi.setSystemTime(new Date("2026-07-30T12:00:00Z"));
    const unix = Date.parse("2026-03-05T15:00:00Z") / 1000;
    expect(compactDate(unix)).toBe("Mar 5");
  });
  it("renders YYYY-MM-DD for a prior year", () => {
    vi.useFakeTimers();
    vi.setSystemTime(new Date("2026-07-30T12:00:00Z"));
    const unix = Date.parse("2025-07-30T12:00:00Z") / 1000;
    expect(compactDate(unix)).toBe("2025-07-30");
  });
});
