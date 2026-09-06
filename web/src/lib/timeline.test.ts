import { describe, expect, it } from "vitest";
import {
  brushFromIndices,
  brushSummary,
  buildLanes,
  dayAxis,
  indexOfDay,
  laneEventsInBrush,
  MAX_AXIS_DAYS,
  PIVOT_ID_CAP,
  pivotUrl,
  planPivot,
  TRACK_ORDER,
  type CanvasLanes,
} from "./timeline";
import { dayBoundsUnix } from "./calendar";
import type { TimelineResponse } from "../api/generated/TimelineResponse";
import type { TimelineTrack } from "../api/generated/TimelineTrack";

// Golden fixture window: 2026-07-01 .. 2026-07-05 (5 UTC days), built off
// the SHIPPED calendar bounds so the test can never drift from the module's
// own day math.
const FROM = dayBoundsUnix("2026-07-01").from;
const TO = dayBoundsUnix("2026-07-05").to;

function lane(
  track: TimelineTrack,
  days: Array<[string, number]>,
  extra: { ids?: string[]; truncated?: boolean; label?: string } = {},
) {
  return {
    track,
    label: extra.label ?? `${track} lane`,
    days: days.map(([day, count]) => ({ day, count })),
    total: days.reduce((a, [, c]) => a + c, 0),
    ids: extra.ids ?? [],
    truncated: extra.truncated ?? false,
  };
}

function response(lanes: TimelineResponse["lanes"]): TimelineResponse {
  return { from: FROM, to: TO, lanes };
}

describe("dayAxis", () => {
  it("is inclusive at both ends, ascending, UTC", () => {
    expect(dayAxis(FROM, TO)).toEqual([
      "2026-07-01",
      "2026-07-02",
      "2026-07-03",
      "2026-07-04",
      "2026-07-05",
    ]);
  });

  it("covers a single day when from and to land inside it", () => {
    expect(dayAxis(FROM, FROM + 60)).toEqual(["2026-07-01"]);
  });

  it("is empty when to precedes from, or on non-finite input", () => {
    expect(dayAxis(TO, FROM)).toEqual([]);
    expect(dayAxis(Number.NaN, TO)).toEqual([]);
  });

  it("never exceeds the hard axis cap", () => {
    expect(dayAxis(0, 10_000 * 86400).length).toBe(MAX_AXIS_DAYS);
  });
});

describe("buildLanes", () => {
  it("returns the four tracks in canonical order even for an empty response", () => {
    const built = buildLanes(null, FROM, TO);
    expect(built.lanes.map((l) => l.track)).toEqual([...TRACK_ORDER]);
    expect(built.days).toHaveLength(5);
    expect(built.lanes.every((l) => l.cells.length === 5)).toBe(true);
    // An absent lane says so rather than presenting a confident zero track.
    expect(built.lanes[0].present).toBe(false);
    expect(built.lanes[0].label).toMatch(/not reported/);
  });

  it("fills EVERY day in the window, including days the wire omitted", () => {
    const built = buildLanes(
      response([lane("created", [["2026-07-03", 4]])]),
      FROM,
      TO,
    );
    const created = built.lanes[0];
    expect(created.cells.map((c) => c.day)).toEqual(built.days);
    expect(created.cells.map((c) => c.count)).toEqual([0, 0, 4, 0, 0]);
    expect(created.total).toBe(4);
  });

  it("keeps the four lanes synchronized on one shared axis", () => {
    const built = buildLanes(
      response([
        lane("created", [["2026-07-01", 1]]),
        lane("read", [["2026-07-05", 2]]),
      ]),
      FROM,
      TO,
    );
    const axes = built.lanes.map((l) => l.cells.map((c) => c.day));
    for (const a of axes) expect(a).toEqual(built.days);
  });

  // Honesty rule 1 — the whole reason this isn't one shared max.
  it("scales density per track INDEPENDENTLY, so a quiet lane stays visible", () => {
    const built = buildLanes(
      response([
        lane("read", [
          ["2026-07-01", 300],
          ["2026-07-02", 150],
        ]),
        lane("comment", [
          ["2026-07-01", 3],
          ["2026-07-02", 1],
        ]),
      ]),
      FROM,
      TO,
    );
    const read = built.lanes.find((l) => l.track === "read")!;
    const comment = built.lanes.find((l) => l.track === "comment")!;
    expect(read.max).toBe(300);
    expect(comment.max).toBe(3);
    // A 3-comment day is the comment lane's own maximum → top band, exactly
    // like the 300-open day is in the read lane. Under one shared maximum it
    // would have been level 0 (invisible).
    expect(comment.cells[0].level).toBe(4);
    expect(read.cells[0].level).toBe(4);
    expect(comment.cells[1].level).toBeGreaterThan(0);
  });

  it("carries the server's own label, ids and truncated flag through", () => {
    const built = buildLanes(
      response([
        lane("session", [], {
          label: "work sessions captured on this daemon (empty outside a sessions corpus)",
          ids: ["s1"],
          truncated: true,
        }),
      ]),
      FROM,
      TO,
    );
    const session = built.lanes.find((l) => l.track === "session")!;
    expect(session.label).toMatch(/empty outside a sessions corpus/);
    expect(session.ids).toEqual(["s1"]);
    expect(session.truncated).toBe(true);
    expect(session.present).toBe(true);
  });

  it("clamps a negative wire count to zero rather than inverting the ramp", () => {
    const built = buildLanes(response([lane("created", [["2026-07-01", -5]])]), FROM, TO);
    expect(built.lanes[0].cells[0].count).toBe(0);
    expect(built.lanes[0].total).toBe(0);
  });
});

describe("brushFromIndices", () => {
  const days = dayAxis(FROM, TO);

  it("normalises a reversed drag", () => {
    const b = brushFromIndices(4, 1, days)!;
    expect([b.fromIndex, b.toIndex]).toEqual([1, 4]);
    expect(b.fromDay).toBe("2026-07-02");
    expect(b.toDay).toBe("2026-07-05");
  });

  it("clamps out-of-window indices to the axis", () => {
    const b = brushFromIndices(-99, 999, days)!;
    expect([b.fromIndex, b.toIndex]).toEqual([0, days.length - 1]);
  });

  it("returns inclusive unix bounds spanning whole UTC days", () => {
    const b = brushFromIndices(0, 0, days)!;
    expect(b.fromUnix).toBe(dayBoundsUnix("2026-07-01").from);
    expect(b.toUnix).toBe(dayBoundsUnix("2026-07-01").to);
    expect(b.days).toEqual(["2026-07-01"]);
  });

  it("is null on an empty axis", () => {
    expect(brushFromIndices(0, 3, [])).toBeNull();
  });

  it("summarises as a plain day count + range, with no achievement framing", () => {
    expect(brushSummary(brushFromIndices(0, 4, days)!)).toBe(
      "5 days · 2026-07-01 → 2026-07-05 (UTC)",
    );
    expect(brushSummary(brushFromIndices(2, 2, days)!)).toBe(
      "1 day · 2026-07-03 → 2026-07-03 (UTC)",
    );
  });
});

describe("indexOfDay", () => {
  const days = dayAxis(FROM, TO);
  it("resolves a day inside the window", () => {
    expect(indexOfDay("2026-07-03", days)).toBe(2);
  });
  it("refuses a day outside the window rather than clamping", () => {
    expect(indexOfDay("2020-01-01", days)).toBeNull();
  });
});

describe("laneEventsInBrush", () => {
  it("sums exactly the brushed days", () => {
    const built = buildLanes(
      response([
        lane("created", [
          ["2026-07-01", 1],
          ["2026-07-03", 2],
          ["2026-07-05", 4],
        ]),
      ]),
      FROM,
      TO,
    );
    const brush = brushFromIndices(1, 3, built.days)!;
    expect(laneEventsInBrush(built.lanes[0], brush)).toBe(2);
  });
});

describe("planPivot", () => {
  const built = (over: Partial<Record<TimelineTrack, ReturnType<typeof lane>>> = {}) =>
    buildLanes(
      response([
        over.created ?? lane("created", [["2026-07-01", 2]], { ids: ["a", "b"] }),
        over.read ?? lane("read", [["2026-07-02", 1]], { ids: ["b", "c"] }),
        over.session ?? lane("session", [], { ids: [] }),
        over.comment ?? lane("comment", [], { ids: [] }),
      ]),
      FROM,
      TO,
    );

  const fullBrush = (l: CanvasLanes) => brushFromIndices(0, l.days.length - 1, l.days)!;

  it("unions the selected lanes' ids, deduped, in canonical track order", () => {
    const l = built();
    const plan = planPivot(l, ["read", "created"], fullBrush(l));
    expect(plan).toEqual({ kind: "ids", ids: ["a", "b", "c"], tracks: ["created", "read"] });
  });

  it("builds the pivot URL through galleryUrl's shipped ids atom only", () => {
    const l = built();
    const plan = planPivot(l, ["created"], fullBrush(l));
    expect(pivotUrl("canon", plan)).toBe("/?kb=canon&ids=a%2Cb");
  });

  it("blocks when no track is selected", () => {
    const l = built();
    const plan = planPivot(l, [], fullBrush(l));
    expect(plan.kind).toBe("blocked");
  });

  it("blocks (never silently truncates) when a MIXED brush is truncated", () => {
    const l = built({ read: lane("read", [], { ids: ["b"], truncated: true }) });
    const plan = planPivot(l, ["created", "read"], fullBrush(l));
    expect(plan.kind).toBe("blocked");
    if (plan.kind === "blocked") {
      expect(plan.reason).toMatch(/capped the read id set/);
      expect(plan.reason).toMatch(/Narrow the brush/);
    }
  });

  it("falls back to the exact mtime window for a CREATION-ONLY truncated brush", () => {
    const l = built({
      created: lane("created", [["2026-07-01", 9]], { ids: ["a"], truncated: true }),
    });
    const brush = brushFromIndices(1, 3, l.days)!;
    const plan = planPivot(l, ["created"], brush);
    expect(plan).toMatchObject({
      kind: "window",
      fromUnix: brush.fromUnix,
      toUnix: brush.toUnix,
    });
    expect(pivotUrl("canon", plan)).toBe(
      `/?kb=canon&from=${brush.fromUnix}&to=${brush.toUnix}`,
    );
  });

  it("degrades over the 500-id gallery cap the same way", () => {
    const many = Array.from({ length: PIVOT_ID_CAP + 1 }, (_, i) => `id${i}`);
    const l = built({ created: lane("created", [["2026-07-01", 1]], { ids: many }) });
    expect(planPivot(l, ["created"], fullBrush(l)).kind).toBe("window");
    expect(planPivot(l, ["created", "read"], fullBrush(l)).kind).toBe("blocked");
  });

  it("stays under the cap at exactly the cap", () => {
    const many = Array.from({ length: PIVOT_ID_CAP }, (_, i) => `id${i}`);
    const l = built({ created: lane("created", [["2026-07-01", 1]], { ids: many }) });
    expect(planPivot(l, ["created"], fullBrush(l)).kind).toBe("ids");
  });

  it("blocks with a plain sentence when the selected tracks recorded nothing", () => {
    const l = built();
    const plan = planPivot(l, ["session"], fullBrush(l));
    expect(plan.kind).toBe("blocked");
    if (plan.kind === "blocked") expect(plan.reason).toMatch(/nothing recorded/);
    expect(pivotUrl("canon", plan)).toBeNull();
  });
});
