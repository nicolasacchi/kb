import { describe, expect, it } from "vitest";
import { isPseudoPath, PSEUDO_NAMES, pseudoNameFromPath, pseudoPath } from "./pseudoFiles";

describe("PSEUDO_NAMES", () => {
  it("has exactly the four reserved names, in reading order", () => {
    expect(PSEUDO_NAMES).toEqual(["pr-body.md", "review.md", "findings.json", "commits.md"]);
  });
});

describe("pseudoPath / isPseudoPath / pseudoNameFromPath", () => {
  it("round-trips every reserved name", () => {
    for (const name of PSEUDO_NAMES) {
      const path = pseudoPath(name);
      expect(path).toBe(`~review/${name}`);
      expect(isPseudoPath(path)).toBe(true);
      expect(pseudoNameFromPath(path)).toBe(name);
    }
  });

  it("a real diffed file path is never mistaken for a pseudo one", () => {
    expect(isPseudoPath("app/models/order.rb")).toBe(false);
    expect(pseudoNameFromPath("app/models/order.rb")).toBeNull();
  });

  it("is total on an empty string", () => {
    expect(isPseudoPath("")).toBe(false);
    expect(pseudoNameFromPath("")).toBeNull();
  });
});
