import { describe, expect, it } from "vitest";
import { parseCapturedParam } from "./capturedParam";

describe("parseCapturedParam()", () => {
  it("splits kb:source_relative on the first colon", () => {
    expect(parseCapturedParam("canon:capture/shared-page-171234.html")).toEqual({
      kb: "canon",
      sourceRelative: "capture/shared-page-171234.html",
    });
  });

  it("keeps any further colons on the source_relative side", () => {
    expect(parseCapturedParam("canon:capture/note:with:colons.md")).toEqual({
      kb: "canon",
      sourceRelative: "capture/note:with:colons.md",
    });
  });

  it("returns null for null/empty input", () => {
    expect(parseCapturedParam(null)).toBeNull();
    expect(parseCapturedParam("")).toBeNull();
  });

  it("returns null for malformed input (no colon, leading colon, trailing colon)", () => {
    expect(parseCapturedParam("no-colon-here")).toBeNull();
    expect(parseCapturedParam(":leading-colon")).toBeNull();
    expect(parseCapturedParam("trailing-colon:")).toBeNull();
  });
});
