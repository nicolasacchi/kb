import { describe, expect, it, vi } from "vitest";
import { PROV_HUES } from "@kb/ui/themes/derive";
import { provHueIndex } from "./provHue";
import {
  ageNow,
  ageText,
  attribute,
  buildMarks,
  fadeLevel,
  rollup,
  summarize,
  worktreeCounts,
  type MirrorTouch,
  type PresenceFileWire,
  type PresenceSnapshot,
} from "./presence";

function file(o: Partial<PresenceFileWire>): PresenceFileWire {
  return {
    repo: "r",
    path: "src/a.rs",
    session_id: "3f2a9c",
    harness: "claude",
    state: "working",
    age_secs: 2,
    ...o,
  };
}
function snap(files: PresenceFileWire[], fetchedPerf = 100_000, available = true): PresenceSnapshot {
  return { schema: "kbc-presence/1", available, now_unix: 0, files, fetchedPerf };
}
const touch = (path: string, at: number): MirrorTouch => ({ repo: "r", path, at });

describe("fadeLevel", () => {
  it("steps at 60 / 180 / 600 seconds", () => {
    expect([59, 60, 179, 180, 599, 600].map((a) => fadeLevel(a, "working"))).toEqual([3, 2, 2, 1, 1, 0]);
  });
  it("caps by state", () => {
    expect(fadeLevel(1, "stalled")).toBe(2);
    for (const s of ["waiting", "cold", "finished", "presumed_ended"]) expect(fadeLevel(1, s)).toBe(1);
    expect(fadeLevel(1, "working")).toBe(3);
    expect(fadeLevel(100, "stalled")).toBe(2);
    expect(fadeLevel(1, "")).toBe(3);
  });
});

describe("ageNow", () => {
  it("adds monotonic elapsed time to the stated age and never reads the wall clock", () => {
    const spy = vi.spyOn(Date, "now").mockReturnValue(9e15);
    expect(ageNow(10, 1000, 6000)).toBe(15);
    expect(ageNow(10, 1000, 6000)).toBe(15);
    spy.mockReturnValue(0);
    expect(ageNow(10, 1000, 6000)).toBe(15);
    spy.mockRestore();
  });
});

describe("attribute", () => {
  it("exact: a beat on the same path within 15 s of the touch", () => {
    // beat at fetchedPerf - 2 s = 98_000; touch at 90_000 -> 8 s apart.
    const [a] = attribute([touch("src/a.rs", 90_000)], snap([file({})]));
    expect(a.trust).toBe("exact");
    expect(a.sessionId).toBe("3f2a9c");
  });
  it("a beat 16 s away is not exact", () => {
    const [a] = attribute([touch("src/a.rs", 81_000)], snap([file({ state: "waiting" })]));
    expect(a.trust).toBe("neutral");
  });
  it("exact picks the nearest beat and counts the other session", () => {
    const [a] = attribute(
      [touch("src/a.rs", 98_000)],
      snap([file({ session_id: "aaaa11", age_secs: 8 }), file({ session_id: "bbbb22", age_secs: 2 })]),
    );
    expect(a.sessionId).toBe("bbbb22");
    expect(a.others).toBe(1);
  });
  it("candidate: no beat on the path but exactly one working session", () => {
    const [a] = attribute([touch("src/other.rs", 99_000)], snap([file({})]));
    expect(a.trust).toBe("candidate");
    expect(a.sessionId).toBe("3f2a9c");
  });
  it("two working sessions means no candidate", () => {
    const [a] = attribute(
      [touch("src/other.rs", 99_000)],
      snap([file({}), file({ session_id: "bbbb22", path: "src/b.rs" })]),
    );
    expect(a.trust).toBe("neutral");
    expect(a.sessionId).toBeUndefined();
  });
  it("nothing: neutral without a snapshot or when unavailable", () => {
    expect(attribute([touch("x", 1)], undefined)[0].trust).toBe("neutral");
    expect(attribute([touch("x", 1)], snap([file({})], 100_000, false))[0].trust).toBe("neutral");
  });
  it("a frozen attribution is returned as is, whatever the current snapshot says", () => {
    const [frozen] = attribute([touch("src/z.rs", 98_000)], undefined);
    expect(frozen.trust).toBe("neutral");
    const t = { ...touch("src/z.rs", 98_000), attr: frozen };
    // exactly one working session now: an unfrozen touch would become a candidate
    expect(attribute([touch("src/z.rs", 98_000)], snap([file({})]))[0].trust).toBe("candidate");
    expect(attribute([t], snap([file({})]))[0].trust).toBe("neutral");
  });
  it("worktree rows never attribute the main checkout's file", () => {
    const [a] = attribute([touch("src/a.rs", 98_000)], snap([file({ worktree: "wt" })]));
    expect(a.trust).toBe("neutral");
  });
});

describe("buildMarks", () => {
  const never = () => undefined;
  it("fades by age, then holds at level 1 and unopened until the ring expires", () => {
    const att = attribute([touch("a", 0)], undefined);
    const at = (secs: number) => buildMarks(att, never, secs * 1000).get("a");
    expect(at(10)?.level).toBe(3);
    expect(at(120)?.level).toBe(2);
    expect(at(300)?.level).toBe(1);
    expect(at(1500)?.level).toBe(1);
    expect(at(1500)?.unopened).toBe(true);
    expect(at(1500)?.label).toBe("Changed 25 min ago, not opened since");
    expect(at(3599)).toBeDefined();
    expect(at(3600)).toBeUndefined();
  });
  it("opening the file after the touch clears it; a later touch lights it again", () => {
    const att = attribute([touch("a", 1000)], undefined);
    expect(buildMarks(att, () => 2000, 5000).size).toBe(0);
    expect(buildMarks(att, () => 500, 5000).size).toBe(1);
  });
  it("labels each trust in its own words", () => {
    const ex = buildMarks(attribute([touch("src/a.rs", 98_000)], snap([file({})])), never, 118_000);
    expect(ex.get("src/a.rs")?.label).toBe("Edited by claude session 3f2a 20 s ago");
    const ca = buildMarks(attribute([touch("src/z.rs", 98_000)], snap([file({})])), never, 118_000);
    expect(ca.get("src/z.rs")?.label).toBe("Probably claude session 3f2a, changed 20 s ago");
    const ne = buildMarks(attribute([touch("src/z.rs", 98_000)], undefined), never, 118_000);
    expect(ne.get("src/z.rs")?.label).toBe("Changed 20 s ago");
  });
  it("a waiting session's exact mark is capped at level 1", () => {
    const m = buildMarks(
      attribute([touch("src/a.rs", 98_000)], snap([file({ state: "waiting" })])),
      never,
      99_000,
    );
    expect(m.get("src/a.rs")?.level).toBe(1);
  });
  it("the hue for a session is provHueIndex(session_id) in range", () => {
    const i = provHueIndex("3f2a9c");
    expect(i).toBeGreaterThanOrEqual(0);
    expect(i).toBeLessThan(PROV_HUES);
  });
});

describe("rollup", () => {
  it("rolls files up into every ancestor with the freshest level and a distinct count", () => {
    const att = attribute(
      [touch("a/b/c.rs", 0), touch("a/b/d.rs", 100_000), touch("a/e.rs", 0)],
      undefined,
    );
    const marks = buildMarks(att, () => undefined, 110_000);
    const r = rollup(marks);
    expect(r.get("a")?.count).toBe(3);
    expect(r.get("a/b")?.count).toBe(2);
    expect(r.get("a")?.level).toBe(3); // d.rs is 10 s old
    expect(r.get("a/b")?.label).toContain("2 changed files inside");
    expect(r.has("a/b/c.rs")).toBe(false);
  });
});

describe("worktreeCounts and summarize", () => {
  it("counts worktree files per worktree, apart from the main checkout", () => {
    const s = snap([
      file({ worktree: "wt1", path: "x.rs" }),
      file({ worktree: "wt1", path: "y.rs", session_id: "bbbb22" }),
      file({ path: "main.rs" }),
    ]);
    expect(worktreeCounts(s, 100_000)).toEqual([{ worktree: "wt1", files: 2 }]);
  });
  it("summarises sessions working and files in the window, null when quiet", () => {
    const s = snap([file({}), file({ path: "b.rs" }), file({ path: "old.rs", age_secs: 700, state: "cold", session_id: "zz" })]);
    expect(summarize(s, 100_000)?.text).toBe("1 session working · 2 files in 10 min");
    expect(summarize(snap([]), 100_000)).toBeNull();
    expect(summarize(undefined, 1)).toBeNull();
    expect(summarize(snap([file({})], 100_000, false), 1)).toBeNull();
  });
});

describe("ageText", () => {
  it("renders seconds, minutes and hours", () => {
    expect([5, 90, 1500, 7300].map(ageText)).toEqual(["5 s ago", "1 min ago", "25 min ago", "2 h ago"]);
  });
});
