import { describe, expect, it } from "vitest";
import type { ReviewComment, ReviewFinding } from "../../api/types";
import { matchesFindingFilters, threadReaderHref } from "./ReviewThreadsCard";

function comment(overrides: Partial<ReviewComment> = {}): ReviewComment {
  return {
    id: "c1",
    path: "src/lib.rs",
    intent: "note",
    body: "look here",
    author: "you",
    created_at: 1000,
    updated_at: 1000,
    resolved: false,
    anchor_kind: "line",
    side: "new",
    ps_number: 1,
    resolution: {
      line: 5,
      orphaned: false,
      resolved_against: { ps: 1, sha: "abc" },
    },
    suggestion: null,
    replies: [],
    ...overrides,
  };
}

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

// V80-M4 — "open in reader": the reader-route sibling of `threadHref`'s
// "open in diff".
describe("threadReaderHref", () => {
  it("is null when the tip sha is not known yet", () => {
    expect(threadReaderHref("acme/widgets", 7, comment(), undefined)).toBeNull();
  });

  it("is null for the review-level General thread (path === \"\")", () => {
    expect(threadReaderHref("acme/widgets", 7, comment({ path: "" }), "deadbeef")).toBeNull();
  });

  it("lands on the thread's own line at the tip sha, with ?review= appended", () => {
    const href = threadReaderHref("acme/widgets", 7, comment({ path: "src/lib.rs" }), "deadbeef");
    expect(href).toBe("/r/acme%2Fwidgets/src/lib.rs?ref=deadbeef&line=5&review=7");
  });

  it("omits the line for an orphaned thread — never guesses a stale line at the current tip", () => {
    const orphan = comment({
      resolution: {
        line: 5,
        orphaned: true,
        resolved_against: { ps: 1, sha: "abc" },
        original: { ps: 1, side: "new", line: 5, snippet: "old line" },
      },
    });
    const href = threadReaderHref("acme/widgets", 7, orphan, "deadbeef");
    expect(href).toBe("/r/acme%2Fwidgets/src/lib.rs?ref=deadbeef&review=7");
  });

  it("omits the line when the resolution carries none", () => {
    const noLine = comment({ resolution: { line: null, orphaned: false, resolved_against: { ps: 1, sha: "abc" } } });
    const href = threadReaderHref("acme/widgets", 7, noLine, "deadbeef");
    expect(href).toBe("/r/acme%2Fwidgets/src/lib.rs?ref=deadbeef&review=7");
  });
});
