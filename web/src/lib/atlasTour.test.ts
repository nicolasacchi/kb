import { describe, expect, it } from "vitest";
import {
  stepStop,
  tourSkippedCount,
  tourStops,
  type TourEntry,
  type TourPoint,
} from "./atlasTour";

const points = (m: Record<string, [number, number]>): Map<string, TourPoint> =>
  new Map(Object.entries(m).map(([id, [x, y]]) => [id, { x, y }]));

const entry = (
  id: string,
  artifactId: string,
  extra: Partial<TourEntry> = {},
): TourEntry => ({
  id,
  artifact_id: artifactId,
  source_relative: `${artifactId}.html`,
  title: `T ${artifactId}`,
  ...extra,
});

describe("tourStops", () => {
  it("is empty for an empty list", () => {
    expect(tourStops([], points({ a: [1, 2] }))).toEqual([]);
  });

  it("is empty when no entry's artifact is on the map", () => {
    const entries = [entry("e1", "a"), entry("e2", "b")];
    expect(tourStops(entries, new Map())).toEqual([]);
    expect(tourSkippedCount(entries, new Map())).toBe(2);
  });

  it("skips the misses and keeps the hits, in list order", () => {
    const entries = [
      entry("e1", "a"),
      entry("e2", "gone"), // not drawn on this atlas
      entry("e3", "b"),
      entry("e4", "c", { tombstone: true }), // artifact left lance
      entry("e5", "d", { source_relative: null }), // nothing to open
      entry("e6", "e"),
    ];
    const stops = tourStops(
      entries,
      points({ a: [10, 20], b: [30, 40], c: [1, 1], d: [2, 2], e: [50, 60] }),
    );
    expect(stops.map((s) => s.entryId)).toEqual(["e1", "e3", "e6"]);
    // `index` counts stops, not list positions — no gaps in "stop 2 of 3".
    expect(stops.map((s) => s.index)).toEqual([0, 1, 2]);
    expect(stops[1]).toMatchObject({ artifactId: "b", x: 30, y: 40, label: "T b" });
    expect(tourSkippedCount(entries, points({ a: [10, 20], b: [30, 40], e: [50, 60] }))).toBe(3);
  });

  it("preserves the list's own order, never re-sorts", () => {
    const entries = [entry("e3", "c"), entry("e1", "a"), entry("e2", "b")];
    const p = points({ a: [1, 1], b: [2, 2], c: [3, 3] });
    expect(tourStops(entries, p).map((s) => s.artifactId)).toEqual(["c", "a", "b"]);
  });

  it("falls back to the source path when an entry has no title", () => {
    const stops = tourStops(
      [entry("e1", "a", { title: null })],
      points({ a: [0, 0] }),
    );
    expect(stops[0].label).toBe("a.html");
  });

  it("is deterministic — same input, same output", () => {
    const entries = [entry("e1", "a"), entry("e2", "x"), entry("e3", "b")];
    const p = points({ a: [1, 2], b: [3, 4] });
    expect(tourStops(entries, p)).toEqual(tourStops(entries, p));
  });
});

describe("stepStop", () => {
  const stops = tourStops(
    [entry("e1", "a"), entry("e2", "b"), entry("e3", "c")],
    points({ a: [0, 0], b: [1, 1], c: [2, 2] }),
  );

  it("steps forward and back without wrapping", () => {
    expect(stepStop(stops, 0, 1)).toBe(1);
    expect(stepStop(stops, 2, 1)).toBe(2);
    expect(stepStop(stops, 1, -1)).toBe(0);
    expect(stepStop(stops, 0, -1)).toBe(0);
  });

  it("clamps into range and handles an empty sequence", () => {
    expect(stepStop(stops, 9, 1)).toBe(2);
    expect(stepStop([], 3, 1)).toBe(0);
  });
});
