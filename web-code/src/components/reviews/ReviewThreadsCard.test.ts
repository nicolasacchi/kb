import { describe, expect, it } from "vitest";
import type { ReviewFinding } from "../../api/types";
import { matchesFindingFilters } from "./ReviewThreadsCard";

function finding(overrides: Partial<ReviewFinding> = {}): ReviewFinding {
  return {
    slug: "f-x",
    severity: "concern",
    category: "cat",
    location: { kind: "whole_file", path: "a.rb", lines: null, removed: false },
    title: "t",
    rationale: "r",
    recommendation: null,
    evidence: null,
    origin: "import",
    author: "claude",
    disposition: null,
    published_state: "unpublished",
    published_at: null,
    published_url: null,
    superseded: false,
    superseded_reason: null,
    content_updated_at: null,
    annotation_id: "a1",
    import_batch_id: "b1",
    created_at: 1,
    updated_at: 1,
    resolution: { line: null, line_end: null, orphaned: false, confidence: "exact" },
    thread_count: 0,
    unresolved_count: 0,
    ...overrides,
  };
}

describe("matchesFindingFilters", () => {
  it("excludes a superseded finding regardless of filters", () => {
    expect(matchesFindingFilters(finding({ superseded: true }), "all", "all")).toBe(false);
  });

  it("'all'/'all' matches everything non-superseded", () => {
    expect(matchesFindingFilters(finding({ severity: "blocker" }), "all", "all")).toBe(true);
  });

  it("filters by exact severity", () => {
    expect(matchesFindingFilters(finding({ severity: "blocker" }), "blocker", "all")).toBe(true);
    expect(matchesFindingFilters(finding({ severity: "concern" }), "blocker", "all")).toBe(false);
  });

  it("'open' disposition filter matches only an undecided (null) disposition", () => {
    expect(matchesFindingFilters(finding({ disposition: null }), "all", "open")).toBe(true);
    expect(
      matchesFindingFilters(
        finding({ disposition: { state: "agree", note: null, by: null, at: null } }),
        "all",
        "open",
      ),
    ).toBe(false);
  });

  it("filters by exact disposition state", () => {
    const disputed = finding({ disposition: { state: "dispute", note: null, by: null, at: null } });
    expect(matchesFindingFilters(disputed, "all", "dispute")).toBe(true);
    expect(matchesFindingFilters(disputed, "all", "agree")).toBe(false);
  });

  it("combines severity AND disposition filters", () => {
    const f = finding({
      severity: "blocker",
      disposition: { state: "waive", note: null, by: null, at: null },
    });
    expect(matchesFindingFilters(f, "blocker", "waive")).toBe(true);
    expect(matchesFindingFilters(f, "concern", "waive")).toBe(false);
    expect(matchesFindingFilters(f, "blocker", "agree")).toBe(false);
  });
});
