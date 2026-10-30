import { beforeEach, describe, expect, it } from "vitest";
import { ATTRIBUTION_GRACE_MS, BURST_PATHS, BURST_SECS, attribute, type PresenceSnapshot } from "./presence";
import {
  burstFor,
  freezeTouches,
  setCurrentFile,
  markOpened,
  openedAtFor,
  presenceVersion,
  recordMirror,
  resetPresenceStore,
  touchesFor,
} from "./presenceStore";

describe("presence store", () => {
  beforeEach(() => resetPresenceStore());

  it("records one touch per named path, newest wins", () => {
    recordMirror("r", ["a", "b"], 10);
    recordMirror("r", ["a"], 20);
    const t = touchesFor("r");
    expect(t.map((x) => [x.path, x.at]).sort()).toEqual([["a", 20], ["b", 10]]);
    expect(touchesFor("other")).toEqual([]);
  });

  it("a batch over the burst limit is a burst, never per-path marks", () => {
    const many = Array.from({ length: BURST_PATHS + 1 }, (_, i) => `f${i}`);
    recordMirror("r", many, 5);
    expect(touchesFor("r")).toEqual([]);
    expect(burstFor("r")?.count).toBe(BURST_PATHS + 1);
    recordMirror("r", Array.from({ length: BURST_PATHS }, (_, i) => `g${i}`), 6);
    expect(touchesFor("r")).toHaveLength(BURST_PATHS);
  });

  it("forgets touches older than the one-hour ring", () => {
    recordMirror("r", ["old"], 0);
    recordMirror("r", ["new"], 3_600_001);
    expect(touchesFor("r").map((t) => t.path)).toEqual(["new"]);
  });

  it("the seen-set keeps the latest open and bumps the version only on change", () => {
    markOpened("r", "a", 100);
    const v = presenceVersion();
    markOpened("r", "a", 50);
    expect(presenceVersion()).toBe(v);
    expect(openedAtFor("r")("a")).toBe(100);
    expect(openedAtFor("r")("b")).toBeUndefined();
    expect(openedAtFor("other")("a")).toBeUndefined();
  });

  it("ignores frames without a repo or paths", () => {
    recordMirror("", ["a"], 1);
    recordMirror("r", [], 1);
    expect(touchesFor("r")).toEqual([]);
  });

  it("the burst line expires and an ordinary touch replaces it", () => {
    const many = Array.from({ length: BURST_PATHS + 1 }, (_, i) => `f${i}`);
    recordMirror("r", many, 1000);
    expect(burstFor("r", 1000 + BURST_SECS * 1000 - 1)?.count).toBe(BURST_PATHS + 1);
    expect(burstFor("r", 1000 + BURST_SECS * 1000)).toBeUndefined();
    recordMirror("r", many, 2000);
    recordMirror("r", ["a"], 2100);
    expect(burstFor("r", 2100)).toBeUndefined();
  });

  it("a touch on the file the operator has open is seen as it lands", () => {
    setCurrentFile("r", "a");
    recordMirror("r", ["a", "b"], 50);
    expect(openedAtFor("r")("a")).toBe(50);
    expect(openedAtFor("r")("b")).toBeUndefined();
  });

  const snap = (fetchedPerf: number, working: boolean): PresenceSnapshot => ({
    schema: "kbc-presence/1",
    available: true,
    now_unix: 0,
    fetchedPerf,
    files: working
      ? [{ repo: "r", path: "elsewhere", session_id: "3f2a9c11", harness: "claude", state: "working", age_secs: 0 }]
      : [],
  });

  it("attribution is frozen: a neutral touch stays neutral when a session starts working later", () => {
    recordMirror("r", ["a"], 1000);
    // Before the grace nothing freezes.
    freezeTouches("r", snap(1000 + ATTRIBUTION_GRACE_MS - 1, false));
    expect(touchesFor("r")[0].attr).toBeUndefined();
    freezeTouches("r", snap(1000 + ATTRIBUTION_GRACE_MS, false));
    expect(touchesFor("r")[0].attr?.trust).toBe("neutral");
    // Minutes later exactly one session is working: the old mark must not change.
    freezeTouches("r", snap(500_000, true));
    expect(attribute(touchesFor("r"), snap(500_000, true))[0].trust).toBe("neutral");
  });
});
