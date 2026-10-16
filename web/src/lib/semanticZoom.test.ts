import { describe, expect, it } from "vitest";
import { feedWheel, nextView, ZOOM_IDLE, ZOOM_SETTLE_MS, ZOOM_THRESHOLD, type ZoomDir } from "./semanticZoom";

const always = () => true;

describe("nextView", () => {
  it("walks atlas -> list -> grid and back", () => {
    expect(nextView("atlas", "in")).toBe("list");
    expect(nextView("list", "in")).toBe("grid");
    expect(nextView("grid", "out")).toBe("list");
    expect(nextView("list", "out")).toBe("atlas");
  });
  it("stops at both ends", () => {
    expect(nextView("grid", "in")).toBeNull();
    expect(nextView("atlas", "out")).toBeNull();
  });
  it("is off the ladder for timeline, canvas, press and junk", () => {
    for (const v of ["history", "canvas", "press", "nonsense", ""]) {
      expect(nextView(v, "in")).toBeNull();
      expect(nextView(v, "out")).toBeNull();
    }
  });
});

describe("feedWheel", () => {
  it("swallows small deltas until the threshold, then steps once", () => {
    let r = feedWheel(ZOOM_IDLE, -ZOOM_THRESHOLD / 4, 1000, always);
    expect(r.action).toBe("swallow");
    r = feedWheel(r.state, -ZOOM_THRESHOLD / 4, 1016, always);
    expect(r.action).toBe("swallow");
    r = feedWheel(r.state, -ZOOM_THRESHOLD / 2, 1032, always);
    expect(r.action).toBe("step");
    expect(r.dir).toBe("in");
  });

  it("takes at most one step per gesture", () => {
    let r = feedWheel(ZOOM_IDLE, -100, 1000, always);
    expect(r.action).toBe("step");
    let t = 1000;
    for (let i = 0; i < 20; i++) {
      t += 16;
      r = feedWheel(r.state, -100, t, always);
      expect(r.action).toBe("swallow");
    }
  });

  it("swallows the tail of a step even at the end of the ladder", () => {
    let r = feedWheel(ZOOM_IDLE, -100, 1000, always);
    expect(r.action).toBe("step");
    r = feedWheel(r.state, -100, 1016, () => false);
    expect(r.action).toBe("swallow");
  });

  it("a new gesture after the settle window steps again", () => {
    let r = feedWheel(ZOOM_IDLE, -100, 1000, always);
    r = feedWheel(r.state, -100, 1000 + ZOOM_SETTLE_MS + 1, always);
    expect(r.action).toBe("step");
  });

  it("passes the event through when no step is available in that direction", () => {
    const onlyOut = (d: ZoomDir) => d === "out";
    const r = feedWheel(ZOOM_IDLE, -100, 1000, onlyOut);
    expect(r.action).toBe("pass");
    expect(feedWheel(ZOOM_IDLE, 100, 1000, onlyOut).action).toBe("step");
  });

  it("positive deltaY steps out", () => {
    const r = feedWheel(ZOOM_IDLE, 100, 1000, always);
    expect(r).toMatchObject({ action: "step", dir: "out" });
  });

  it("a quiet gap or a direction flip restarts the accumulator", () => {
    let r = feedWheel(ZOOM_IDLE, -30, 1000, always);
    r = feedWheel(r.state, -30, 1000 + ZOOM_SETTLE_MS + 50, always);
    expect(r.action).toBe("swallow"); // 30 + 30 would have stepped without the reset
    let f = feedWheel(ZOOM_IDLE, -30, 1000, always);
    f = feedWheel(f.state, 30, 1010, always);
    expect(f.action).toBe("swallow");
    expect(f.state.acc).toBe(30);
  });

  it("ignores a zero delta", () => {
    expect(feedWheel(ZOOM_IDLE, 0, 1000, always).action).toBe("pass");
  });
});
