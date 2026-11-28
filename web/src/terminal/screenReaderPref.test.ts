// @vitest-environment jsdom
import { beforeEach, describe, expect, it } from "vitest";
import { readScreenReader, writeScreenReader, SCREEN_READER_KEY } from "./screenReaderPref";

beforeEach(() => window.localStorage.clear());

describe("screenReaderPref", () => {
  it("defaults to off", () => {
    expect(readScreenReader()).toBe(false);
  });
  it("only a stored 1 is on", () => {
    window.localStorage.setItem(SCREEN_READER_KEY, "1");
    expect(readScreenReader()).toBe(true);
    window.localStorage.setItem(SCREEN_READER_KEY, "true");
    expect(readScreenReader()).toBe(false);
    window.localStorage.setItem(SCREEN_READER_KEY, "0");
    expect(readScreenReader()).toBe(false);
  });
  it("round-trips", () => {
    writeScreenReader(true);
    expect(readScreenReader()).toBe(true);
    writeScreenReader(false);
    expect(readScreenReader()).toBe(false);
  });
  it("throwing storage degrades to off and never throws", () => {
    const bad = {
      getItem() {
        throw new Error("blocked");
      },
      setItem() {
        throw new Error("blocked");
      },
    };
    expect(readScreenReader(bad)).toBe(false);
    expect(() => writeScreenReader(true, bad)).not.toThrow();
    expect(readScreenReader(null)).toBe(false);
  });
});
