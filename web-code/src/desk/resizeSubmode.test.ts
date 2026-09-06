import { describe, expect, it } from "vitest";
import {
  RESIZE_FAR,
  RESIZE_STEP,
  RESIZE_SUBMODE_HINT,
  resizeSubmodeKey,
  type DeskFocusRegion,
} from "./resizeSubmode";

const one = { paneCount: 1 } as const;

describe("resize submode — exit / equalise / hint", () => {
  for (const key of ["Escape", "q", "Enter"]) {
    it(`${key} exits`, () => {
      expect(resizeSubmodeKey(key, { focus: "main", ...one })).toEqual({ t: "exit" });
    });
  }
  it("= equalises", () => {
    expect(resizeSubmodeKey("=", { focus: "main", ...one })).toEqual({ t: "equalise" });
    expect(resizeSubmodeKey("+", { focus: "main", ...one })).toEqual({ t: "equalise" });
  });
  it("? asks for the one-line hint", () => {
    expect(resizeSubmodeKey("?", { focus: "dock", ...one })).toEqual({ t: "hint" });
    expect(RESIZE_SUBMODE_HINT).toContain("h/j/k/l");
    expect(RESIZE_SUBMODE_HINT).toContain("= equalise");
  });
});

describe("resize submode — the region graph decides direction, not the focused edge", () => {
  it("the dock grows rightward and the rail grows leftward — the same key, opposite senses", () => {
    expect(resizeSubmodeKey("l", { focus: "dock", ...one })).toEqual({
      t: "resize",
      target: "dock",
      delta: RESIZE_STEP,
    });
    expect(resizeSubmodeKey("l", { focus: "rail", ...one })).toEqual({
      t: "resize",
      target: "rail",
      delta: -RESIZE_STEP,
    });
    expect(resizeSubmodeKey("h", { focus: "rail", ...one })).toEqual({
      t: "resize",
      target: "rail",
      delta: RESIZE_STEP,
    });
  });

  it("from main, a direction shrinks the neighbour that owns that boundary", () => {
    expect(resizeSubmodeKey("h", { focus: "main", ...one })).toEqual({
      t: "resize",
      target: "dock",
      delta: -RESIZE_STEP,
    });
    expect(resizeSubmodeKey("l", { focus: "main", ...one })).toEqual({
      t: "resize",
      target: "rail",
      delta: -RESIZE_STEP,
    });
    expect(resizeSubmodeKey("j", { focus: "main", ...one })).toEqual({
      t: "resize",
      target: "drawer",
      delta: -RESIZE_STEP,
    });
    expect(resizeSubmodeKey("k", { focus: "main", ...one })).toEqual({
      t: "resize",
      target: "drawer",
      delta: RESIZE_STEP,
    });
  });

  it("with two panes open, horizontal keys in main address the pane split instead", () => {
    expect(resizeSubmodeKey("l", { focus: "main", paneCount: 2 })).toEqual({
      t: "resize",
      target: "panes",
      delta: RESIZE_STEP,
    });
    expect(resizeSubmodeKey("h", { focus: "main", paneCount: 2 })).toEqual({
      t: "resize",
      target: "panes",
      delta: -RESIZE_STEP,
    });
    // Vertical keys still reach the drawer — the split only claims the
    // horizontal axis.
    expect(resizeSubmodeKey("k", { focus: "main", paneCount: 2 })).toEqual({
      t: "resize",
      target: "drawer",
      delta: RESIZE_STEP,
    });
  });

  it("the drawer only moves vertically", () => {
    expect(resizeSubmodeKey("k", { focus: "drawer", ...one })).toEqual({
      t: "resize",
      target: "drawer",
      delta: RESIZE_STEP,
    });
    expect(resizeSubmodeKey("j", { focus: "drawer", ...one })).toEqual({
      t: "resize",
      target: "drawer",
      delta: -RESIZE_STEP,
    });
    expect(resizeSubmodeKey("h", { focus: "drawer", ...one })).toEqual({ t: "noop" });
    expect(resizeSubmodeKey("l", { focus: "drawer", ...one })).toEqual({ t: "noop" });
  });

  it("a direction with no boundary that way is a no-op, never a guess at another separator", () => {
    expect(resizeSubmodeKey("j", { focus: "dock", ...one })).toEqual({ t: "noop" });
    expect(resizeSubmodeKey("k", { focus: "rail", ...one })).toEqual({ t: "noop" });
  });
});

describe("resize submode — magnitudes and aliases", () => {
  it("shifted keys step far, in the same direction", () => {
    expect(resizeSubmodeKey("L", { focus: "dock", ...one })).toEqual({
      t: "resize",
      target: "dock",
      delta: RESIZE_FAR,
    });
    expect(resizeSubmodeKey("H", { focus: "dock", ...one })).toEqual({
      t: "resize",
      target: "dock",
      delta: -RESIZE_FAR,
    });
    expect(RESIZE_FAR).toBeGreaterThan(RESIZE_STEP);
  });

  it("arrow keys mirror hjkl", () => {
    for (const [arrow, letter] of [
      ["ArrowLeft", "h"],
      ["ArrowRight", "l"],
      ["ArrowUp", "k"],
      ["ArrowDown", "j"],
    ]) {
      expect(resizeSubmodeKey(arrow, { focus: "main", ...one })).toEqual(
        resizeSubmodeKey(letter, { focus: "main", ...one }),
      );
    }
  });

  it("is total — every key produces a command, so the submode can swallow them all", () => {
    const focuses: DeskFocusRegion[] = ["dock", "main", "rail", "drawer"];
    const keys = ["a", "Z", "1", "F5", " ", "Tab", "x", "%", "Backspace"];
    for (const focus of focuses) {
      for (const k of keys) {
        expect(resizeSubmodeKey(k, { focus, ...one })).toEqual({ t: "noop" });
      }
    }
  });
});
