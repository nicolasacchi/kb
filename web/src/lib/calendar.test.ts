import { describe, expect, it } from "vitest";
import {
  buildCalendarGrid,
  cellTooltip,
  dayBoundsUnix,
  densityLevel,
  unixToUtcDay,
  type CalendarDayCount,
} from "./calendar";

describe("densityLevel", () => {
  it("is 0 for a non-positive count or a non-positive max", () => {
    expect(densityLevel(0, 10)).toBe(0);
    expect(densityLevel(-1, 10)).toBe(0);
    expect(densityLevel(5, 0)).toBe(0);
  });

  it("buckets into quartile bands of (0, max]", () => {
    expect(densityLevel(1, 4)).toBe(1); // 0.25, not > 0.25
    expect(densityLevel(2, 4)).toBe(2); // 0.5, not > 0.5
    expect(densityLevel(3, 4)).toBe(3); // 0.75, not > 0.75
    expect(densityLevel(4, 4)).toBe(4); // 1.0
  });
});

describe("buildCalendarGrid", () => {
  it("fills every day in [from, to], zero-counting absent days", () => {
    const grid = buildCalendarGrid("2021-01-01", "2021-01-10", []);
    expect(grid.cells).toHaveLength(10);
    expect(grid.cells.every((c) => c.total === 0)).toBe(true);
    expect(grid.cells.every((c) => c.level === 0)).toBe(true);
    expect(grid.totalEvents).toBe(0);
  });

  it("computes UTC weekday + Sunday-start week columns (2021-01-01 is a Friday)", () => {
    const grid = buildCalendarGrid("2021-01-01", "2021-01-10", []);
    const byDay = new Map(grid.cells.map((c) => [c.day, c]));
    expect(byDay.get("2021-01-01")).toMatchObject({ weekday: 5, week: 0 }); // Fri
    expect(byDay.get("2021-01-02")).toMatchObject({ weekday: 6, week: 0 }); // Sat
    expect(byDay.get("2021-01-03")).toMatchObject({ weekday: 0, week: 1 }); // Sun — new column
    expect(byDay.get("2021-01-09")).toMatchObject({ weekday: 6, week: 1 }); // Sat
    expect(byDay.get("2021-01-10")).toMatchObject({ weekday: 0, week: 2 }); // Sun — new column
    expect(grid.weeks).toBe(3);
  });

  it("labels a month only at its first in-range Sunday column", () => {
    const grid = buildCalendarGrid("2021-01-01", "2021-01-10", []);
    // No Dec Sunday is in range (grid starts mid-week on a Friday), so the
    // partial leading column (week 0) gets no label — only Jan's first
    // in-range Sunday (2021-01-03, week 1) does.
    expect(grid.monthLabels).toEqual([{ week: 1, label: "Jan" }]);
  });

  it("assigns density levels relative to the grid's own max", () => {
    const days: CalendarDayCount[] = [
      { day: "2021-01-05", opens: 4, searches: 0, comments: 0 },
      { day: "2021-01-06", opens: 1, searches: 0, comments: 0 },
    ];
    const grid = buildCalendarGrid("2021-01-01", "2021-01-10", days);
    const byDay = new Map(grid.cells.map((c) => [c.day, c]));
    expect(byDay.get("2021-01-05")?.level).toBe(4); // 4/4
    expect(byDay.get("2021-01-06")?.level).toBe(1); // 1/4 = 0.25, not > 0.25
    expect(byDay.get("2021-01-07")?.level).toBe(0); // untouched day
    expect(grid.totalEvents).toBe(5);
  });

  it("sums opens+searches+comments into total, per day", () => {
    const days: CalendarDayCount[] = [
      { day: "2021-01-05", opens: 2, searches: 1, comments: 3 },
    ];
    const grid = buildCalendarGrid("2021-01-05", "2021-01-05", days);
    expect(grid.cells).toEqual([
      {
        day: "2021-01-05",
        weekday: 2, // Tue
        week: 0,
        total: 6,
        opens: 2,
        searches: 1,
        comments: 3,
        level: 4,
      },
    ]);
  });

  it("returns an empty grid when from > to", () => {
    const grid = buildCalendarGrid("2021-01-10", "2021-01-01", []);
    expect(grid.cells).toEqual([]);
    expect(grid.weeks).toBe(0);
    expect(grid.monthLabels).toEqual([]);
    expect(grid.totalEvents).toBe(0);
  });

  it("is stable across UTC-day boundaries a local-parsed Date would shift", () => {
    // A regression guard for the "never new Date(str) local-parse" rule:
    // if this ever regressed to local parsing, running the suite in a
    // negative-UTC-offset TZ would shift day boundaries by one day. Vitest
    // pins TZ=UTC (vitest.config.ts), so this mostly documents the
    // contract — the UTC-explicit build above is what keeps it honest.
    const grid = buildCalendarGrid("2021-12-31", "2022-01-01", []);
    expect(grid.cells.map((c) => c.day)).toEqual(["2021-12-31", "2022-01-01"]);
  });
});

describe("dayBoundsUnix", () => {
  it("returns the UTC [00:00:00, 23:59:59] bounds for a day", () => {
    // 2021-01-01T00:00:00Z = 1609459200 (cross-checked against the
    // kb-core sqlite fixtures using the same epoch).
    expect(dayBoundsUnix("2021-01-01")).toEqual({
      from: 1_609_459_200,
      to: 1_609_545_599,
    });
  });
});

describe("unixToUtcDay", () => {
  it("round-trips with dayBoundsUnix", () => {
    expect(unixToUtcDay(1_609_459_200)).toBe("2021-01-01");
    expect(unixToUtcDay(dayBoundsUnix("2021-06-15").from)).toBe("2021-06-15");
  });
});

describe("cellTooltip", () => {
  it("labels the day as UTC and pluralizes each kind correctly", () => {
    const grid = buildCalendarGrid("2021-01-05", "2021-01-05", [
      { day: "2021-01-05", opens: 1, searches: 2, comments: 0 },
    ]);
    expect(cellTooltip(grid.cells[0])).toBe(
      "2021-01-05 (UTC) — 1 open, 2 searches, 0 comments",
    );
  });
});
