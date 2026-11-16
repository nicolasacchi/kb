import { describe, expect, it } from "vitest";
import { ImageDefStore } from "./imageDefs";
import type { ImageDefFrame } from "../wire";

const SHA_A = "a".repeat(64);
const SHA_B = "b".repeat(64);

function frame(id: number, sha = SHA_A, over: Partial<ImageDefFrame> = {}): ImageDefFrame {
  return {
    t: "imagedef", seq: 1, id, gen: 1, sha256: sha, mime: "image/png",
    w: 400, h: 200, cols: 40, rows: 10, offset: 0, ...over,
  };
}

describe("gfx ImageDefStore", () => {
  it("identical retransmit is no change", () => {
    const store = new ImageDefStore();
    let events = 0;
    store.onChange(() => { events += 1; });
    expect(store.apply(frame(7))).toBe(true);
    expect(events).toBe(1);
    expect(store.get(7)?.version).toBe(1);
    // Same bytes, same box, even with a new seq/gen/offset: nothing happens.
    expect(store.apply(frame(7, SHA_A, { seq: 9, gen: 4, offset: 123 }))).toBe(false);
    expect(events).toBe(1);
    expect(store.get(7)?.version).toBe(1);
  });

  it("different bytes bump version", () => {
    const store = new ImageDefStore();
    let events = 0;
    store.onChange(() => { events += 1; });
    store.apply(frame(7));
    store.apply(frame(7, SHA_B, { gen: 2 }));
    expect(events).toBe(2);
    const def = store.get(7);
    expect(def?.version).toBe(2);
    expect(def?.sha).toBe(SHA_B);
    expect(def?.gen).toBe(2);
    store.apply(frame(7, SHA_A, { gen: 3 }));
    expect(store.get(7)?.version).toBe(3);
  });

  it("same bytes with a new box updates the box and keeps the version", () => {
    const store = new ImageDefStore();
    let events = 0;
    store.onChange(() => { events += 1; });
    store.apply(frame(7));
    expect(store.apply(frame(7, SHA_A, { cols: 20, rows: 5 }))).toBe(true);
    expect(events).toBe(2);
    expect(store.get(7)).toMatchObject({ cols: 20, rows: 5, version: 1, sha: SHA_A });
  });

  it("gfxReset keeps defs", () => {
    const store = new ImageDefStore();
    let events = 0;
    store.onChange(() => { events += 1; });
    store.apply(frame(1));
    store.apply(frame(2, SHA_B));
    expect(events).toBe(2);
    store.gfxReset(5);
    expect(store.size).toBe(2);
    expect(store.get(1)?.version).toBe(1);
    expect(store.resets).toBe(1);
    expect(store.resetGen).toBe(5);
    expect(events).toBe(2);
    // An identical retransmit after the reset costs nothing.
    expect(store.apply(frame(1))).toBe(false);
    expect(store.get(1)?.version).toBe(1);
  });

  it("clear drops defs only on full join", () => {
    const store = new ImageDefStore();
    let events = 0;
    store.apply(frame(1));
    store.onChange(() => { events += 1; });
    store.gfxReset(2);
    expect(store.size).toBe(1);
    store.clear();
    expect(store.size).toBe(0);
    expect(events).toBe(1);
    store.clear();
    expect(events).toBe(1);
    // After a clear the same bytes are a NEW def again (version 1).
    store.apply(frame(1));
    expect(store.get(1)?.version).toBe(1);
  });

  it("unknown id lookup is undefined", () => {
    const store = new ImageDefStore();
    expect(store.get(404)).toBeUndefined();
    expect(store.all()).toEqual([]);
  });

  it("onChange returns an unsubscribe", () => {
    const store = new ImageDefStore();
    let events = 0;
    const off = store.onChange(() => { events += 1; });
    store.apply(frame(1));
    off();
    store.apply(frame(2, SHA_B));
    expect(events).toBe(1);
  });
});
