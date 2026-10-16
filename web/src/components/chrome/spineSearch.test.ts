import { describe, expect, it } from "vitest";
import { spineHasSearch } from "./Header";

describe("spineHasSearch", () => {
  it("omits the Spine launcher on the Search page only", () => {
    expect(spineHasSearch("/search")).toBe(false);
    expect(spineHasSearch("/")).toBe(true);
    expect(spineHasSearch("/memory")).toBe(true);
    expect(spineHasSearch("/a/canon/x.html")).toBe(true);
  });
});
