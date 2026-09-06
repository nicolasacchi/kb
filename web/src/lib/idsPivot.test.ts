import { describe, expect, it } from "vitest";
import { GALLERY_IDS_CAP, groupIdsByKb, isOverIdsCap } from "./idsPivot";

describe("groupIdsByKb", () => {
  // invariant:35
  it("groups by kb, preserving first-seen kb and id order", () => {
    expect(
      groupIdsByKb([
        { kb: "research", id: "a1" },
        { kb: "code", id: "c1" },
        { kb: "research", id: "a2" },
      ]),
    ).toEqual([
      { kb: "research", ids: ["a1", "a2"] },
      { kb: "code", ids: ["c1"] },
    ]);
  });

  it("dedupes a repeated id within one kb (e.g. a file read twice)", () => {
    expect(
      groupIdsByKb([
        { kb: "research", id: "a1" },
        { kb: "research", id: "a1" },
      ]),
    ).toEqual([{ kb: "research", ids: ["a1"] }]);
  });

  it("drops pairs missing a kb or id", () => {
    expect(
      groupIdsByKb([
        { kb: "", id: "a1" },
        { kb: "research", id: "" },
        { kb: "research", id: "a2" },
      ]),
    ).toEqual([{ kb: "research", ids: ["a2"] }]);
  });

  it("returns an empty array for an empty input", () => {
    expect(groupIdsByKb([])).toEqual([]);
  });
});

describe("isOverIdsCap", () => {
  it("is false at and under the cap, true over it", () => {
    expect(isOverIdsCap(Array.from({ length: GALLERY_IDS_CAP }, (_, i) => `id${i}`))).toBe(
      false,
    );
    expect(
      isOverIdsCap(Array.from({ length: GALLERY_IDS_CAP + 1 }, (_, i) => `id${i}`)),
    ).toBe(true);
  });

  it("is false for an empty set", () => {
    expect(isOverIdsCap([])).toBe(false);
  });
});
