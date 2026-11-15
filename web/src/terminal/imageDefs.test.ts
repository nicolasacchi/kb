import { describe, expect, it } from "vitest";
import { ImageDefStore } from "./imageDefs";
import type { ImageDefFrame } from "./wire";

function def(id: number, gen = 1): ImageDefFrame {
  return {
    t: "imagedef", seq: 0, id, gen, sha256: `sha${id}`, mime: "image/png",
    w: 8, h: 8, cols: 1, rows: 1, offset: 0,
  };
}

describe("ImageDefStore", () => {
  it("dedupes by id (later def wins) and sorts by id", () => {
    const store = new ImageDefStore();
    store.put(def(2));
    store.put(def(1));
    store.put(def(2, 3));
    expect(store.size).toBe(2);
    expect(store.get(2)?.gen).toBe(3);
    expect(store.all().map((d) => d.id)).toEqual([1, 2]);
  });
});
