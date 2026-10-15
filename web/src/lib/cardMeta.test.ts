import { describe, expect, it } from "vitest";
import { cardMetaParts, readMinutes } from "./cardMeta";

describe("cardMetaParts", () => {
  it("reading time is words/220, rounded, never below one minute", () => {
    expect(readMinutes(220)).toBe(1);
    expect(readMinutes(5)).toBe(1);
    expect(readMinutes(1100)).toBe(5);
    expect(readMinutes(1210)).toBe(6);
  });
  it("is absent without a word count", () => {
    expect(readMinutes(0)).toBeNull();
    expect(readMinutes(null)).toBeNull();
    expect(readMinutes(undefined)).toBeNull();
    expect(cardMetaParts("a/b", 0).read).toBeNull();
  });
  it("carries folder and the N min read label", () => {
    expect(cardMetaParts("research/x", 1100)).toEqual({
      folder: "research/x",
      read: "5 min read",
    });
    expect(cardMetaParts("", 220).folder).toBeNull();
  });
});
