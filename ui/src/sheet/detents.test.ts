import { describe, expect, it } from "vitest";
import { next, type Detent } from "./detents";

const P = 300;
const F = 700;
const base = { peekPx: P, fullPx: F };
const states: Detent[] = ["closed", "peek", "full"];

describe("detent machine", () => {
  it("open: closed -> peek, otherwise unchanged", () => {
    expect(next("closed", { type: "open", ...base })).toBe("peek");
    expect(next("peek", { type: "open", ...base })).toBe("peek");
    expect(next("full", { type: "open", ...base })).toBe("full");
  });
  it("close: every state -> closed", () => {
    for (const s of states) expect(next(s, { type: "close", ...base })).toBe("closed");
  });
  it("up steps closed -> peek -> full -> full", () => {
    expect(next("closed", { type: "up", ...base })).toBe("peek");
    expect(next("peek", { type: "up", ...base })).toBe("full");
    expect(next("full", { type: "up", ...base })).toBe("full");
  });
  it("down steps full -> peek -> closed -> closed", () => {
    expect(next("full", { type: "down", ...base })).toBe("peek");
    expect(next("peek", { type: "down", ...base })).toBe("closed");
    expect(next("closed", { type: "down", ...base })).toBe("closed");
  });
  it("dragEnd with no offset keeps the state", () => {
    for (const s of states) expect(next(s, { type: "dragEnd", ...base })).toBe(s);
  });
  it("dragEnd snaps to the nearest detent by offset (positive = down)", () => {
    const d = (s: Detent, offset: number) => next(s, { type: "dragEnd", offset, ...base });
    // from full (700): height 700 - offset
    expect(d("full", 100)).toBe("full"); // 600, nearest full
    expect(d("full", 250)).toBe("peek"); // 450, 150 from peek vs 250 from full
  });
  it("dragEnd boundary cases", () => {
    const d = (s: Detent, offset: number) => next(s, { type: "dragEnd", offset, ...base });
    expect(d("full", 600)).toBe("closed");
    expect(d("peek", 160)).toBe("closed"); // 140 < 150 midpoint
    expect(d("peek", 100)).toBe("peek");
    expect(d("peek", -250)).toBe("full"); // 550
    expect(d("peek", -100)).toBe("peek"); // 400
    expect(d("closed", -200)).toBe("peek"); // 200
    expect(d("closed", -50)).toBe("closed");
  });
  it("fling beyond 0.5 px/ms forces direction", () => {
    const f = (s: Detent, velocity: number, offset = 0) =>
      next(s, { type: "dragEnd", velocity, offset, ...base });
    expect(f("peek", 0.6)).toBe("closed"); // downward fling from peek closes
    expect(f("full", 0.6)).toBe("peek");
    expect(f("peek", -0.6)).toBe("full");
    expect(f("closed", -0.6)).toBe("peek");
    expect(f("closed", 0.6)).toBe("closed");
    // velocity wins over a tiny opposite offset
    expect(f("peek", 0.9, -50)).toBe("closed");
  });
  it("velocity at or below the threshold falls back to offset snapping", () => {
    expect(next("peek", { type: "dragEnd", velocity: 0.5, offset: 0, ...base })).toBe("peek");
    expect(next("peek", { type: "dragEnd", velocity: -0.5, offset: 0, ...base })).toBe("peek");
  });
});
