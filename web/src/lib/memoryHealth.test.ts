import { describe, expect, it } from "vitest";
import { anyHealth } from "./memoryHealth";

describe("anyHealth", () => {
  it("is false when no memory has been recalled (the column would be empty)", () => {
    expect(anyHealth([{ recall_count: 0 }, { recall_count: 0 }])).toBe(false);
    expect(anyHealth([])).toBe(false);
  });
  it("is true as soon as one row has health data", () => {
    expect(anyHealth([{ recall_count: 0 }, { recall_count: 1 }])).toBe(true);
  });
});
