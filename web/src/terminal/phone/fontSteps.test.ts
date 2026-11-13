// @vitest-environment jsdom
import { afterEach, describe, expect, it, vi } from "vitest";
import { FONT_KEY, readFont, writeFont } from "./fontSteps";

afterEach(() => {
  window.localStorage.clear();
  vi.restoreAllMocks();
});

describe("persisted phone font", () => {
  it("round-trips a font step", () => {
    writeFont(16);
    expect(window.localStorage.getItem(FONT_KEY)).toBe("16");
    expect(readFont()).toBe(16);
  });

  it("ignores a value that is not a step and falls back to 12", () => {
    window.localStorage.setItem(FONT_KEY, "15");
    expect(readFont()).toBe(12);
    window.localStorage.setItem(FONT_KEY, "abc");
    expect(readFont()).toBe(12);
    writeFont(15);
    expect(window.localStorage.getItem(FONT_KEY)).toBe("abc");
  });

  it("survives a blocked storage (private mode)", () => {
    vi.spyOn(Storage.prototype, "getItem").mockImplementation(() => {
      throw new Error("blocked");
    });
    vi.spyOn(Storage.prototype, "setItem").mockImplementation(() => {
      throw new Error("blocked");
    });
    expect(readFont()).toBe(12);
    expect(() => writeFont(18)).not.toThrow();
  });
});
