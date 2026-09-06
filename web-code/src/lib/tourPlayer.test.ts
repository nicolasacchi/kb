import { describe, expect, it } from "vitest";
import { parseTourStepParam } from "./tourPlayer";

describe("parseTourStepParam", () => {
  it("defaults to 0 (the first span) for null/missing", () => {
    expect(parseTourStepParam(null)).toBe(0);
  });

  it("parses a 1-based step into a 0-based index", () => {
    expect(parseTourStepParam("1")).toBe(0);
    expect(parseTourStepParam("3")).toBe(2);
  });

  it("defaults to 0 for junk/empty/zero/negative", () => {
    expect(parseTourStepParam("")).toBe(0);
    expect(parseTourStepParam("0")).toBe(0);
    expect(parseTourStepParam("-2")).toBe(0);
    expect(parseTourStepParam("abc")).toBe(0);
    expect(parseTourStepParam("NaN")).toBe(0);
  });

  it("accepts a leading-zero step", () => {
    expect(parseTourStepParam("007")).toBe(6);
  });
});
