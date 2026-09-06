import { describe, expect, it } from "vitest";
import { prNumberFromRef, prRef } from "./prRef";

describe("prNumberFromRef", () => {
  it("extracts the PR number from a fetched-PR ref", () => {
    expect(prNumberFromRef("refs/kbc/pr/42")).toBe(42);
  });

  it("returns null for an ordinary branch/tag ref", () => {
    expect(prNumberFromRef("main")).toBeNull();
    expect(prNumberFromRef("refs/heads/main")).toBeNull();
    expect(prNumberFromRef("refs/tags/v1.0")).toBeNull();
  });

  it("returns null for a sha", () => {
    expect(prNumberFromRef("a".repeat(40))).toBeNull();
  });

  it("returns null for a malformed kbc pr ref", () => {
    expect(prNumberFromRef("refs/kbc/pr/")).toBeNull();
    expect(prNumberFromRef("refs/kbc/pr/abc")).toBeNull();
    expect(prNumberFromRef("refs/kbc/pr/42/head")).toBeNull();
  });
});

describe("prRef", () => {
  it("builds the fetched-PR ref string", () => {
    expect(prRef(42)).toBe("refs/kbc/pr/42");
  });
});

describe("round-trip", () => {
  it("prNumberFromRef(prRef(n)) === n", () => {
    for (const n of [1, 7, 42, 12345]) {
      expect(prNumberFromRef(prRef(n))).toBe(n);
    }
  });
});
