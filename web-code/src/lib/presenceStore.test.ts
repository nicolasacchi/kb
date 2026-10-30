import { beforeEach, describe, expect, it } from "vitest";
import { BURST_PATHS } from "./presence";
import {
  burstFor,
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
});
