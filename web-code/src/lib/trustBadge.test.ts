import { describe, expect, it } from "vitest";
import { isLiveTier, trustTierFrom } from "./trustBadge";

describe("trustTierFrom", () => {
  it("passes through every known tier verbatim", () => {
    expect(trustTierFrom("exact")).toBe("exact");
    expect(trustTierFrom("likely")).toBe("likely");
    expect(trustTierFrom("candidate")).toBe("candidate");
  });

  it("classifies down on an unrecognized value", () => {
    expect(trustTierFrom("bogus-future-tier")).toBe("candidate");
    expect(trustTierFrom("")).toBe("candidate");
  });

  it("classifies down when the class field is missing entirely (older daemon)", () => {
    expect(trustTierFrom(undefined)).toBe("candidate");
    expect(trustTierFrom(null)).toBe("candidate");
  });
});

describe("isLiveTier", () => {
  it("is true only for lsp-live", () => {
    expect(isLiveTier("lsp-live")).toBe(true);
  });

  it("is false for every other precision, including exact-class tiers", () => {
    expect(isLiveTier("scip-exact")).toBe(false);
    expect(isLiveTier("locals")).toBe(false);
    expect(isLiveTier("file-local")).toBe(false);
    expect(isLiveTier("framework-convention")).toBe(false);
    expect(isLiveTier(undefined)).toBe(false);
    expect(isLiveTier(null)).toBe(false);
  });
});
