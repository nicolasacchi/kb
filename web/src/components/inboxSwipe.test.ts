import { describe, expect, it } from "vitest";
import {
  SWIPE_THRESHOLD_PX,
  clampSwipeDelta,
  resolveSwipeAction,
  swipeDirection,
  swipeHintStrength,
} from "./inboxSwipe";

describe("clampSwipeDelta", () => {
  it("passes values inside the range through unchanged", () => {
    expect(clampSwipeDelta(0)).toBe(0);
    expect(clampSwipeDelta(50)).toBe(50);
    expect(clampSwipeDelta(-50)).toBe(-50);
  });

  it("clamps past the max in either direction", () => {
    expect(clampSwipeDelta(500)).toBe(140);
    expect(clampSwipeDelta(-500)).toBe(-140);
  });

  it("honors a custom max", () => {
    expect(clampSwipeDelta(100, 80)).toBe(80);
    expect(clampSwipeDelta(-100, 80)).toBe(-80);
  });
});

describe("swipeDirection", () => {
  it("is right for positive, left for negative, null at rest", () => {
    expect(swipeDirection(1)).toBe("right");
    expect(swipeDirection(-1)).toBe("left");
    expect(swipeDirection(0)).toBe(null);
  });
});

describe("swipeHintStrength", () => {
  it("is 0 at rest and saturates to 1 exactly at the threshold", () => {
    expect(swipeHintStrength(0)).toBe(0);
    expect(swipeHintStrength(SWIPE_THRESHOLD_PX / 2)).toBeCloseTo(0.5);
    expect(swipeHintStrength(SWIPE_THRESHOLD_PX)).toBe(1);
  });

  it("never exceeds 1 past the threshold, either direction", () => {
    expect(swipeHintStrength(SWIPE_THRESHOLD_PX * 2)).toBe(1);
    expect(swipeHintStrength(-SWIPE_THRESHOLD_PX * 2)).toBe(1);
  });
});

describe("resolveSwipeAction", () => {
  // invariant: the threshold is inclusive — a release AT the boundary still
  // fires (matches the hint layer reading "fully revealed" at that point).
  it("fires resolve past the positive threshold, inclusive", () => {
    expect(resolveSwipeAction(SWIPE_THRESHOLD_PX)).toBe("resolve");
    expect(resolveSwipeAction(SWIPE_THRESHOLD_PX + 1)).toBe("resolve");
  });

  it("fires reply past the negative threshold, inclusive", () => {
    expect(resolveSwipeAction(-SWIPE_THRESHOLD_PX)).toBe("reply");
    expect(resolveSwipeAction(-SWIPE_THRESHOLD_PX - 1)).toBe("reply");
  });

  it("cancels (null) below the threshold in either direction", () => {
    expect(resolveSwipeAction(SWIPE_THRESHOLD_PX - 1)).toBe(null);
    expect(resolveSwipeAction(-(SWIPE_THRESHOLD_PX - 1))).toBe(null);
    expect(resolveSwipeAction(0)).toBe(null);
  });

  it("honors a custom threshold", () => {
    expect(resolveSwipeAction(50, 50)).toBe("resolve");
    expect(resolveSwipeAction(49, 50)).toBe(null);
  });
});
