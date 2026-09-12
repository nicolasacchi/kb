import { describe, expect, it } from "vitest";
import {
  labelFor,
  posFromAt,
  posFromRef,
  resolveAsOf,
  resolutionOf,
  shaForPos,
  stepNext,
  stepPrev,
  type ScrubStop,
} from "./scrub";

function s(sha: string, when: number, subject = sha): ScrubStop {
  return {
    sha,
    when,
    author_kind: "none",
    subject,
    insertions: 1,
    deletions: 0,
    path: "a.rs",
  };
}

const STOPS: ScrubStop[] = [s("c3c3c3c3c3c3c3", 300, "n3"), s("c2c2c2c2c2c2c2", 200, "n2"), s("c1c1c1c1c1c1c1", 100, "n1")];
const FLOOR = { sha: "c1c1c1c1c1c1c1", when: 100 };

describe("scrub resolve_as_of (V76-R3d)", () => {
  it("picks nearest-prior, exact on a same-second hit, miss before the floor", () => {
    expect(resolveAsOf(STOPS, 300)?.sha).toBe("c3c3c3c3c3c3c3");
    expect(resolutionOf(resolveAsOf(STOPS, 300)!, 300)).toBe("exact");
    expect(resolveAsOf(STOPS, 250)?.sha).toBe("c2c2c2c2c2c2c2");
    expect(resolutionOf(resolveAsOf(STOPS, 250)!, 250)).toBe("nearest-prior");
    expect(resolveAsOf(STOPS, 100)?.sha).toBe("c1c1c1c1c1c1c1");
    expect(resolveAsOf(STOPS, 99)).toBeUndefined();
    expect(resolveAsOf([], 1)).toBeUndefined();
  });

  it("posFromAt agrees with resolveAsOf", () => {
    expect(posFromAt(STOPS, 300)).toEqual({ kind: "stop", index: 0, resolution: "exact" });
    expect(posFromAt(STOPS, 250)).toEqual({ kind: "stop", index: 1, resolution: "nearest-prior" });
    expect(posFromAt(STOPS, 99)).toEqual({ kind: "before-floor" });
  });
});

describe("scrub stepper", () => {
  it("steps back twice from the working tree, then miss at the floor", () => {
    let p = posFromRef(STOPS, undefined);
    expect(p).toEqual({ kind: "working-tree" });
    p = stepPrev(p, STOPS);
    expect(p).toEqual({ kind: "stop", index: 0, resolution: "nearest-prior" });
    expect(shaForPos(p, STOPS)).toBe("c3c3c3c3c3c3c3");
    p = stepPrev(p, STOPS);
    expect(shaForPos(p, STOPS)).toBe("c2c2c2c2c2c2c2");
    p = stepPrev(p, STOPS);
    expect(shaForPos(p, STOPS)).toBe("c1c1c1c1c1c1c1");
    p = stepPrev(p, STOPS);
    expect(p).toEqual({ kind: "before-floor" });
    p = stepPrev(p, STOPS);
    expect(p).toEqual({ kind: "before-floor" });
  });

  it("steps forward from the miss back to the working tree", () => {
    let p = stepNext({ kind: "before-floor" }, STOPS);
    expect(shaForPos(p, STOPS)).toBe("c1c1c1c1c1c1c1");
    p = stepNext(p, STOPS);
    expect(shaForPos(p, STOPS)).toBe("c2c2c2c2c2c2c2");
    p = stepNext(p, STOPS);
    expect(shaForPos(p, STOPS)).toBe("c3c3c3c3c3c3c3");
    p = stepNext(p, STOPS);
    expect(p).toEqual({ kind: "working-tree" });
    p = stepNext(p, STOPS);
    expect(p).toEqual({ kind: "working-tree" });
  });

  it("a named ref that is not a sha starts at the newest stop", () => {
    expect(posFromRef(STOPS, "main")).toEqual({
      kind: "stop",
      index: 0,
      resolution: "nearest-prior",
    });
    expect(posFromRef(STOPS, "c2c2c2c2c2c2c2")).toEqual({
      kind: "stop",
      index: 1,
      resolution: "exact",
    });
  });
});

describe("scrub labels", () => {
  it("renders nearest-prior, exact, and the floor miss", () => {
    expect(labelFor({ kind: "working-tree" }, STOPS, FLOOR)).toBe("working tree");
    expect(labelFor({ kind: "stop", index: 1, resolution: "nearest-prior" }, STOPS, FLOOR)).toBe(
      "nearest-prior · c2c2c2c2c2c2 · 200",
    );
    expect(labelFor({ kind: "stop", index: 0, resolution: "exact" }, STOPS, FLOOR)).toBe(
      "exact · c3c3c3c3c3c3 · 300",
    );
    expect(labelFor({ kind: "before-floor" }, STOPS, FLOOR)).toBe("before the floor (c1c1c1c1c1c1)");
    expect(labelFor({ kind: "before-floor" }, STOPS, null)).toBe("before the floor (none)");
  });
});
