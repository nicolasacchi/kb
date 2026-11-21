import { describe, expect, it } from "vitest";
import { clipboardAction, inputLabel, scrolledUp, type KeyLike } from "./input";

const key = (o: Partial<KeyLike>): KeyLike => ({
  type: "keydown",
  key: "c",
  ctrlKey: false,
  metaKey: false,
  shiftKey: false,
  altKey: false,
  ...o,
});

describe("clipboardAction", () => {
  it("Ctrl/Cmd+C copies only with a selection; without one it is ^C", () => {
    expect(clipboardAction(key({ ctrlKey: true }), true)).toBe("copy");
    expect(clipboardAction(key({ metaKey: true }), true)).toBe("copy");
    expect(clipboardAction(key({ ctrlKey: true }), false)).toBe("pass");
    expect(clipboardAction(key({ metaKey: true }), false)).toBe("pass");
  });

  it("Ctrl/Cmd+Shift+C always copies, Ctrl/Cmd+Shift+V always pastes", () => {
    expect(clipboardAction(key({ ctrlKey: true, shiftKey: true }), false)).toBe("copy");
    expect(clipboardAction(key({ ctrlKey: true, shiftKey: true, key: "C" }), true)).toBe("copy");
    expect(clipboardAction(key({ metaKey: true, shiftKey: true, key: "v" }), false)).toBe("paste");
    expect(clipboardAction(key({ ctrlKey: true, shiftKey: true, key: "V" }), true)).toBe("paste");
  });

  it("everything else passes: plain keys, Alt chords, keyup and keypress, other Ctrl keys", () => {
    expect(clipboardAction(key({}), true)).toBe("pass");
    expect(clipboardAction(key({ ctrlKey: true, altKey: true }), true)).toBe("pass");
    expect(clipboardAction(key({ ctrlKey: true, type: "keyup" }), true)).toBe("pass");
    expect(clipboardAction(key({ ctrlKey: true, type: "keypress" }), true)).toBe("pass");
    expect(clipboardAction(key({ ctrlKey: true, key: "d" }), true)).toBe("pass");
    expect(clipboardAction(key({ ctrlKey: true, key: "v" }), true)).toBe("pass");
  });
});

describe("input chip and scroll", () => {
  it("names where keys go", () => {
    expect(inputLabel(true)).toBe("typing");
    expect(inputLabel(false)).toBe("click to type");
  });

  it("scrolled up only when the viewport is above the live bottom", () => {
    expect(scrolledUp({ viewportY: 10, baseY: 100 })).toBe(true);
    expect(scrolledUp({ viewportY: 100, baseY: 100 })).toBe(false);
    expect(scrolledUp({ viewportY: 0, baseY: 0 })).toBe(false);
  });
});
