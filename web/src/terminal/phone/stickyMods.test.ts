// PHIN2 - the sticky Ctrl latch: a pure state machine on an injected clock.

import { describe, expect, it } from "vitest";
import { DOUBLE_TAP_MS, applyCtrl, createStickyMods } from "./stickyMods";

function rig() {
  let t = 1000;
  const mods = createStickyMods(() => t);
  return { mods, advance: (ms: number) => (t += ms) };
}

describe("StickyMods", () => {
  it("off -> armed -> off on two slow taps", () => {
    const { mods, advance } = rig();
    expect(mods.getState()).toBe("off");
    mods.tap();
    expect(mods.getState()).toBe("armed");
    advance(DOUBLE_TAP_MS + 1);
    mods.tap();
    expect(mods.getState()).toBe("off");
  });

  it("a double tap within 350 ms locks; a third tap turns it off", () => {
    const { mods, advance } = rig();
    mods.tap();
    advance(DOUBLE_TAP_MS);
    mods.tap();
    expect(mods.getState()).toBe("locked");
    advance(10);
    mods.tap();
    expect(mods.getState()).toBe("off");
  });

  it("consume is one-shot when armed and persistent when locked; off never applies", () => {
    const { mods, advance } = rig();
    expect(mods.consume()).toBe(false);
    mods.tap();
    expect(mods.consume()).toBe(true);
    expect(mods.getState()).toBe("off");
    expect(mods.consume()).toBe(false);
    mods.tap();
    advance(50);
    mods.tap();
    expect(mods.consume()).toBe(true);
    expect(mods.consume()).toBe(true);
    expect(mods.getState()).toBe("locked");
  });

  it("release is idempotent and notifies only on a change", () => {
    const { mods } = rig();
    let n = 0;
    const un = mods.subscribe(() => (n += 1));
    mods.release();
    expect(n).toBe(0);
    mods.tap();
    expect(n).toBe(1);
    mods.release();
    mods.release();
    expect(mods.getState()).toBe("off");
    expect(n).toBe(2);
    un();
    mods.tap();
    expect(n).toBe(2);
  });
});

describe("applyCtrl", () => {
  it("maps letters, punctuation and the rest of the table", () => {
    expect(applyCtrl("a")).toEqual({ out: "\x01", consumed: true });
    expect(applyCtrl("c")).toEqual({ out: "\x03", consumed: true });
    expect(applyCtrl("Z")).toEqual({ out: "\x1a", consumed: true });
    expect(applyCtrl("[").out).toBe("\x1b");
    expect(applyCtrl("\\").out).toBe("\x1c");
    expect(applyCtrl("]").out).toBe("\x1d");
    expect(applyCtrl("^").out).toBe("\x1e");
    expect(applyCtrl("_").out).toBe("\x1f");
    expect(applyCtrl("@").out).toBe("\x00");
    expect(applyCtrl(" ").out).toBe("\x00");
    expect(applyCtrl("?").out).toBe("\x7f");
  });

  it("passes digits and other characters through, unconsumed", () => {
    expect(applyCtrl("5")).toEqual({ out: "5", consumed: false });
    expect(applyCtrl("é")).toEqual({ out: "é", consumed: false });
    expect(applyCtrl("")).toEqual({ out: "", consumed: false });
  });

  it("touches only the first code point of a longer string", () => {
    expect(applyCtrl("cat")).toEqual({ out: "\x03at", consumed: true });
    expect(applyCtrl("1ab")).toEqual({ out: "1ab", consumed: false });
    expect(applyCtrl("😀c").out).toBe("😀c");
  });
});
