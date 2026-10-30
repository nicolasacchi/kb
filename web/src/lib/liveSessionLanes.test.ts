import { describe, expect, it } from "vitest";
import {
  FINISHED_LANE_CAP,
  blockedCount,
  blockedLabel,
  blockedSince,
  bucketLiveSessions,
  isLongWait,
  isPresumedEnded,
  isStalled,
  LAST_TOOL_TITLE,
  lastToolLabel,
  liveHonestyKind,
  liveHonestyLabel,
  liveHonestyTitle,
  waitingCount,
} from "./liveSessionLanes";
import type { LiveStatusRow } from "../api/sessions";

const NOW = 1_800_000_000; // fixed unix-seconds instant

function row(overrides: Partial<LiveStatusRow> & Pick<LiveStatusRow, "session_id">): LiveStatusRow {
  return {
    harness: "claude",
    holder: "agent",
    state: "working",
    source: "hook",
    confidence: "observed",
    since_unix: NOW,
    since_secs: 0,
    resume: `claude -r ${overrides.session_id}`,
    blocked: false,
    ...overrides,
  };
}

describe("bucketLiveSessions — lane assignment", () => {
  it("assigns working+stalled to inProgress, waiting+cold to waiting, finished+presumed_ended to finished", () => {
    const rows = [
      row({ session_id: "a", state: "working" }),
      row({ session_id: "b", state: "stalled" }),
      row({ session_id: "c", state: "waiting" }),
      row({ session_id: "d", state: "cold" }),
      row({ session_id: "e", state: "finished" }),
      row({ session_id: "f", state: "presumed_ended" }),
    ];
    const lanes = bucketLiveSessions(rows);
    expect(lanes.inProgress.map((r) => r.session_id).sort()).toEqual(["a", "b"]);
    expect(lanes.waiting.map((r) => r.session_id).sort()).toEqual(["c", "d"]);
    expect(lanes.finished.map((r) => r.session_id).sort()).toEqual(["e", "f"]);
  });

  it("collapses entirely to empty lanes when there are no rows", () => {
    const lanes = bucketLiveSessions([]);
    expect(lanes.inProgress).toEqual([]);
    expect(lanes.waiting).toEqual([]);
    expect(lanes.finished).toEqual([]);
    expect(lanes.finishedTotal).toBe(0);
  });
});

describe("bucketLiveSessions — ordering", () => {
  it("in progress: most-recently-active first", () => {
    const rows = [
      row({ session_id: "old", state: "working", since_unix: NOW - 600 }),
      row({ session_id: "newest", state: "working", since_unix: NOW - 10 }),
      row({ session_id: "mid", state: "stalled", since_unix: NOW - 300 }),
    ];
    const { inProgress } = bucketLiveSessions(rows);
    expect(inProgress.map((r) => r.session_id)).toEqual(["newest", "mid", "old"]);
  });

  it("waiting: longest-wait-first (the 'you are the bottleneck' ordering)", () => {
    const rows = [
      row({ session_id: "just-asked", state: "waiting", since_unix: NOW - 60 }),
      row({ session_id: "ancient", state: "cold", since_unix: NOW - 40_000 }),
      row({ session_id: "mid-wait", state: "waiting", since_unix: NOW - 3_000 }),
    ];
    const { waiting } = bucketLiveSessions(rows);
    expect(waiting.map((r) => r.session_id)).toEqual(["ancient", "mid-wait", "just-asked"]);
  });

  it("finished: newest first", () => {
    const rows = [
      row({ session_id: "oldest", state: "finished", since_unix: NOW - 7_200 }),
      row({ session_id: "just-ended", state: "finished", since_unix: NOW - 5 }),
      row({ session_id: "an-hour-ago", state: "presumed_ended", since_unix: NOW - 3_600 }),
    ];
    const { finished } = bucketLiveSessions(rows);
    expect(finished.map((r) => r.session_id)).toEqual([
      "just-ended",
      "an-hour-ago",
      "oldest",
    ]);
  });
});

describe("bucketLiveSessions — finished lane cap", () => {
  it("caps the finished lane and reports the true total", () => {
    const rows = Array.from({ length: 12 }, (_, i) =>
      row({
        session_id: `f${i}`,
        state: "finished",
        since_unix: NOW - i, // i=0 is newest
      }),
    );
    const lanes = bucketLiveSessions(rows);
    expect(lanes.finished).toHaveLength(FINISHED_LANE_CAP);
    expect(lanes.finishedTotal).toBe(12);
    // newest-first survives the cap
    expect(lanes.finished.map((r) => r.session_id)).toEqual([
      "f0",
      "f1",
      "f2",
      "f3",
      "f4",
    ]);
  });

  it("a custom cap is honoured", () => {
    const rows = Array.from({ length: 3 }, (_, i) =>
      row({ session_id: `f${i}`, state: "finished", since_unix: NOW - i }),
    );
    const lanes = bucketLiveSessions(rows, 1);
    expect(lanes.finished).toHaveLength(1);
    expect(lanes.finishedTotal).toBe(3);
  });

  it("in-progress and waiting lanes are never capped", () => {
    const rows = Array.from({ length: 12 }, (_, i) =>
      row({ session_id: `w${i}`, state: "waiting", since_unix: NOW - i }),
    );
    const lanes = bucketLiveSessions(rows, 5);
    expect(lanes.waiting).toHaveLength(12);
  });
});

describe("waitingCount", () => {
  it("matches the waiting lane's length exactly (chip and lane never disagree)", () => {
    const rows = [
      row({ session_id: "a", state: "waiting" }),
      row({ session_id: "b", state: "cold" }),
      row({ session_id: "c", state: "working" }),
    ];
    expect(waitingCount(rows)).toBe(bucketLiveSessions(rows).waiting.length);
    expect(waitingCount(rows)).toBe(2);
  });

  it("is zero for an empty or all-non-waiting fleet", () => {
    expect(waitingCount([])).toBe(0);
    expect(waitingCount([row({ session_id: "a", state: "working" })])).toBe(0);
  });
});

describe("liveHonestyKind — the presumed/capture honesty marker", () => {
  it("returns null for an ordinary hook-observed row (no badge)", () => {
    expect(liveHonestyKind({ confidence: "observed", source: "hook" })).toBeNull();
  });

  it("flags a presumed row even when source is hook", () => {
    expect(liveHonestyKind({ confidence: "presumed", source: "hook" })).toBe("presumed");
  });

  it("flags a capture-sourced row", () => {
    expect(liveHonestyKind({ confidence: "inferred", source: "capture" })).toBe("capture");
  });

  it("presumed takes precedence when a row is both presumed and capture-sourced", () => {
    expect(liveHonestyKind({ confidence: "presumed", source: "capture" })).toBe("presumed");
  });

  it("an inferred, hook/transcript-sourced row is not flagged", () => {
    expect(liveHonestyKind({ confidence: "inferred", source: "transcript" })).toBeNull();
  });

  it("labels and titles are non-empty, distinct strings for each kind", () => {
    expect(liveHonestyLabel("presumed")).toBe("presumed");
    expect(liveHonestyLabel("capture")).toBe("as of last capture");
    expect(liveHonestyTitle("presumed")).not.toBe(liveHonestyTitle("capture"));
  });
});

describe("state-decoration helpers", () => {
  it("isStalled/isLongWait/isPresumedEnded read the state field honestly", () => {
    expect(isStalled(row({ session_id: "a", state: "stalled" }))).toBe(true);
    expect(isStalled(row({ session_id: "a", state: "working" }))).toBe(false);
    expect(isLongWait(row({ session_id: "a", state: "cold" }))).toBe(true);
    expect(isLongWait(row({ session_id: "a", state: "waiting" }))).toBe(false);
    expect(isPresumedEnded(row({ session_id: "a", state: "presumed_ended" }))).toBe(true);
    expect(isPresumedEnded(row({ session_id: "a", state: "finished" }))).toBe(false);
  });
});

describe("OL IDEA1 - the blocked lane", () => {
  it("a blocked working/stalled row goes ONLY to blocked, not inProgress", () => {
    const lanes = bucketLiveSessions([
      row({ session_id: "a", state: "working", blocked: true }),
      row({ session_id: "b", state: "stalled", blocked: true }),
      row({ session_id: "c", state: "working" }),
    ]);
    expect(lanes.blocked.map((r) => r.session_id).sort()).toEqual(["a", "b"]);
    expect(lanes.inProgress.map((r) => r.session_id)).toEqual(["c"]);
  });

  it("sorts longest-blocked first by blocked_since_unix, falling back to since_unix", () => {
    const lanes = bucketLiveSessions([
      row({ session_id: "new", blocked: true, blocked_since_unix: NOW - 10, since_unix: NOW }),
      row({ session_id: "old", blocked: true, blocked_since_unix: NOW - 900, since_unix: NOW }),
      row({ session_id: "legacy", blocked: true, since_unix: NOW - 300 }),
    ]);
    expect(lanes.blocked.map((r) => r.session_id)).toEqual(["old", "legacy", "new"]);
  });

  it("a blocked presumed_ended row stays in finished", () => {
    const lanes = bucketLiveSessions([
      row({ session_id: "x", state: "presumed_ended", holder: "none", blocked: true }),
    ]);
    expect(lanes.blocked).toEqual([]);
    expect(lanes.finished.map((r) => r.session_id)).toEqual(["x"]);
  });

  it("blockedCount counts the lane and leaves waitingCount alone", () => {
    const rows = [
      row({ session_id: "a", blocked: true }),
      row({ session_id: "b", state: "waiting", holder: "human" }),
    ];
    expect(blockedCount(rows)).toBe(1);
    expect(waitingCount(rows)).toBe(1);
  });

  it("blockedSince and blockedLabel", () => {
    expect(blockedSince({ blocked_since_unix: 5, since_unix: 9 })).toBe(5);
    expect(blockedSince({ since_unix: 9 })).toBe(9);
    expect(blockedLabel({ blocked_tool: "Bash", detail_reason: "x" })).toBe("permission: Bash");
    expect(blockedLabel({ detail_reason: "  Claude Code needs your input " })).toBe(
      "Claude Code needs your input",
    );
    expect(blockedLabel({})).toBe("needs you");
    expect(blockedLabel({ detail_reason: "   " })).toBe("needs you");
  });
});

describe("OL IDEA4 - last-tool evidence", () => {
  const NOW_MS = NOW * 1000;

  it("lastToolLabel formats name and age, and is null when either field is missing", () => {
    expect(lastToolLabel({ last_tool: "Bash", last_tool_at: NOW - 38 * 60 }, NOW_MS)).toBe(
      "last: Bash · 38m",
    );
    expect(lastToolLabel({ last_tool: "Bash" }, NOW_MS)).toBeNull();
    expect(lastToolLabel({ last_tool_at: NOW }, NOW_MS)).toBeNull();
    expect(lastToolLabel({}, NOW_MS)).toBeNull();
    expect(lastToolLabel({ last_tool: "", last_tool_at: NOW }, NOW_MS)).toBeNull();
  });

  it("the tooltip says started is not running", () => {
    expect(LAST_TOOL_TITLE).toContain("may have died");
  });

  it("bucketing and ordering are identical with and without the fields", () => {
    const plain = [
      row({ session_id: "a", state: "stalled", since_unix: NOW - 3000 }),
      row({ session_id: "b", state: "working", since_unix: NOW - 10 }),
      row({ session_id: "c", state: "waiting", since_unix: NOW - 500, holder: "human" }),
      row({ session_id: "d", state: "presumed_ended", since_unix: NOW - 40000 }),
      row({ session_id: "e", state: "stalled", since_unix: NOW - 3000, blocked: true }),
    ];
    const withTool = plain.map((r) => ({ ...r, last_tool: "Bash", last_tool_at: r.since_unix }));
    const ids = (rs: ReturnType<typeof bucketLiveSessions>) => ({
      blocked: rs.blocked.map((r) => r.session_id),
      inProgress: rs.inProgress.map((r) => r.session_id),
      waiting: rs.waiting.map((r) => r.session_id),
      finished: rs.finished.map((r) => r.session_id),
      total: rs.finishedTotal,
    });
    expect(ids(bucketLiveSessions(withTool))).toEqual(ids(bucketLiveSessions(plain)));
  });
});
