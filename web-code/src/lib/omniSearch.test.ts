import { describe, expect, it } from "vitest";
import type { ChunkHit, LaneSection } from "../api/types";
import { mergeSemanticFollowup, needsSemanticFollowup, sectionsToRowCounts } from "./omniSearch";

function section(lane: LaneSection["lane"], overrides: Partial<LaneSection> = {}): LaneSection {
  return { lane, results: [], truncated: false, ...overrides };
}

describe("needsSemanticFollowup", () => {
  it("is true when the semantic section is pending", () => {
    expect(needsSemanticFollowup([section("files"), section("semantic", { pending: true })])).toBe(
      true,
    );
  });

  it("is false when semantic already has results (no pending flag)", () => {
    expect(needsSemanticFollowup([section("semantic", { results: [{}] })])).toBe(false);
  });

  it("is false when there is no semantic section at all (a prefix-scoped query)", () => {
    expect(needsSemanticFollowup([section("files")])).toBe(false);
  });
});

describe("mergeSemanticFollowup", () => {
  const hits: ChunkHit[] = [
    { repo: "kb", path: "a.rs", span_start: 1, span_end: 5, score: 0.9, snippet: "fn a" },
  ];

  it("replaces the pending semantic section's results and clears pending", () => {
    const sections = [section("files", { results: [{}] }), section("semantic", { pending: true })];
    const merged = mergeSemanticFollowup(sections, hits);
    const semantic = merged.find((s) => s.lane === "semantic")!;
    expect(semantic.pending).toBe(false);
    expect(semantic.results).toBe(hits);
    // The unrelated section is untouched.
    expect(merged.find((s) => s.lane === "files")!.results).toEqual([{}]);
  });

  it("is a no-op array-shape-wise when there is no semantic section", () => {
    const sections = [section("files", { results: [{}] })];
    expect(mergeSemanticFollowup(sections, hits)).toEqual(sections);
  });
});

describe("sectionsToRowCounts", () => {
  it("orders lanes canonically and counts rows per lane", () => {
    const sections = [
      section("symbols", { results: [{}, {}] }),
      section("files", { results: [{}] }),
    ];
    expect(sectionsToRowCounts(sections)).toEqual([
      { lane: "files", rowCount: 1 },
      { lane: "symbols", rowCount: 2 },
    ]);
  });

  it("zeroes out pending and unavailable sections", () => {
    const sections = [
      section("semantic", { pending: true, results: [{}, {}] }),
      section("sessions", { unavailable_reason: "kb daemon unreachable" }),
    ];
    expect(sectionsToRowCounts(sections)).toEqual([
      { lane: "semantic", rowCount: 0 },
      { lane: "sessions", rowCount: 0 },
    ]);
  });
});
