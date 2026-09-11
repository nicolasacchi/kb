import { describe, expect, it } from "vitest";
import {
  clampMapWidth,
  loadMapWidth,
  REVIEW_MAP_WIDTH_DEFAULT,
  saveMapWidth,
  type StorageLike,
} from "./reviewMapLayout";

function mem(init: Record<string, string> = {}): StorageLike {
  const bag = { ...init };
  return {
    getItem: (k) => (k in bag ? bag[k] : null),
    setItem: (k, v) => {
      bag[k] = v;
    },
  };
}

describe("review map width", () => {
  it("defaults when missing or corrupt", () => {
    expect(loadMapWidth(mem())).toBe(REVIEW_MAP_WIDTH_DEFAULT);
    expect(loadMapWidth(mem({ "kbc:review-map-width": "nope" }))).toBe(REVIEW_MAP_WIDTH_DEFAULT);
    expect(loadMapWidth(mem({ "kbc:review-map-width": "{" }))).toBe(REVIEW_MAP_WIDTH_DEFAULT);
  });

  it("round-trips a clamped percent", () => {
    const s = mem();
    saveMapWidth(s, 30);
    expect(loadMapWidth(s)).toBe(30);
    saveMapWidth(s, 3);
    expect(loadMapWidth(s)).toBe(12);
    saveMapWidth(s, 99);
    expect(loadMapWidth(s)).toBe(48);
  });

  it("clampMapWidth rejects NaN", () => {
    expect(clampMapWidth(Number.NaN)).toBe(REVIEW_MAP_WIDTH_DEFAULT);
  });
});
