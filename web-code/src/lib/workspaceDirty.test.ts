import { describe, expect, it } from "vitest";
import { isWorkingSetDirty } from "./workspaceDirty";

describe("isWorkingSetDirty", () => {
  it("is not dirty when the two orders are identical", () => {
    expect(isWorkingSetDirty(["a.rs", "b.rs"], ["a.rs", "b.rs"])).toBe(false);
  });

  it("is dirty on a different length", () => {
    expect(isWorkingSetDirty(["a.rs"], ["a.rs", "b.rs"])).toBe(true);
    expect(isWorkingSetDirty(["a.rs", "b.rs"], ["a.rs"])).toBe(true);
  });

  it("is dirty when membership differs at the same length", () => {
    expect(isWorkingSetDirty(["a.rs", "c.rs"], ["a.rs", "b.rs"])).toBe(true);
  });

  it("is dirty when order differs even with identical membership", () => {
    expect(isWorkingSetDirty(["b.rs", "a.rs"], ["a.rs", "b.rs"])).toBe(true);
  });

  it("two empty lists are not dirty", () => {
    expect(isWorkingSetDirty([], [])).toBe(false);
  });

  it("empty vs. non-empty is dirty in either direction", () => {
    expect(isWorkingSetDirty([], ["a.rs"])).toBe(true);
    expect(isWorkingSetDirty(["a.rs"], [])).toBe(true);
  });
});
