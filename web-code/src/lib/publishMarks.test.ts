import { describe, expect, it } from "vitest";
import type { ReviewFinding } from "../api/types";
import {
  __resetPublishMarksForTests,
  clearMarks,
  eligibleForPublishMark,
  isMarked,
  markedCount,
  markedSlugs,
  toggleMark,
  toggleMarkedSet,
} from "./publishMarks";

function finding(overrides: Partial<ReviewFinding> = {}): ReviewFinding {
  return {
    slug: "f-a",
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
    annotation_id: "ann1",
    import_batch_id: "batch_1",
    created_at: 1000,
    updated_at: 1000,
    resolution: { line: 1, line_end: null, orphaned: false, confidence: "exact" },
    thread_count: 0,
    unresolved_count: 0,
    ...overrides,
  };
}

describe("toggleMarkedSet", () => {
  it("adds an absent slug", () => {
    const next = toggleMarkedSet(new Set(), "f-a");
    expect(Array.from(next)).toEqual(["f-a"]);
  });

  it("removes a present slug", () => {
    const next = toggleMarkedSet(new Set(["f-a", "f-b"]), "f-a");
    expect(Array.from(next)).toEqual(["f-b"]);
  });

  it("never mutates the input set", () => {
    const input = new Set(["f-a"]);
    toggleMarkedSet(input, "f-b");
    expect(Array.from(input)).toEqual(["f-a"]);
  });
});

describe("the module store", () => {
  it("starts empty for every review", () => {
    __resetPublishMarksForTests();
    expect(markedCount(1)).toBe(0);
    expect(markedSlugs(1)).toEqual([]);
    expect(isMarked(1, "f-a")).toBe(false);
  });

  it("toggles independently per review id", () => {
    __resetPublishMarksForTests();
    toggleMark(1, "f-a");
    toggleMark(2, "f-b");
    expect(markedSlugs(1)).toEqual(["f-a"]);
    expect(markedSlugs(2)).toEqual(["f-b"]);
  });

  it("toggling twice returns to unmarked", () => {
    __resetPublishMarksForTests();
    toggleMark(1, "f-a");
    toggleMark(1, "f-a");
    expect(isMarked(1, "f-a")).toBe(false);
    expect(markedCount(1)).toBe(0);
  });

  // PRR-U8 regression — `useMarkedSlugs` (`PublishPreview.tsx`'s only
  // consumer) feeds `markedSlugs` straight into `useSyncExternalStore` as
  // `getSnapshot`, which requires a STABLE reference across repeated calls
  // when nothing changed (React's own contract; a fresh array every call
  // reads as "the store changed," which crashed `PublishPreview` with
  // "Maximum update depth exceeded" the first time an e2e spec ever
  // exercised it — PRR-U8's own report).
  it("returns the SAME array reference across repeated calls with no mutation", () => {
    __resetPublishMarksForTests();
    toggleMark(1, "f-a");
    const first = markedSlugs(1);
    const second = markedSlugs(1);
    expect(second).toBe(first);
  });

  it("returns a NEW reference only after an actual mutation", () => {
    __resetPublishMarksForTests();
    toggleMark(1, "f-a");
    const before = markedSlugs(1);
    toggleMark(1, "f-b");
    const after = markedSlugs(1);
    expect(after).not.toBe(before);
    expect(after).toEqual(["f-a", "f-b"]);
  });

  it("clearMarks also invalidates the cached reference", () => {
    __resetPublishMarksForTests();
    toggleMark(1, "f-a");
    const before = markedSlugs(1);
    clearMarks(1);
    const after = markedSlugs(1);
    expect(after).not.toBe(before);
    expect(after).toEqual([]);
  });
});

describe("eligibleForPublishMark", () => {
  it("is eligible when undecided (no disposition yet)", () => {
    expect(eligibleForPublishMark(finding({ disposition: null }))).toBe(true);
  });

  it("is eligible when agreed", () => {
    expect(
      eligibleForPublishMark(finding({ disposition: { state: "agree", note: null, by: "you", at: 1 } })),
    ).toBe(true);
  });

  it("is NOT eligible when waived", () => {
    expect(
      eligibleForPublishMark(finding({ disposition: { state: "waive", note: null, by: "you", at: 1 } })),
    ).toBe(false);
  });

  it("is NOT eligible when disputed or deferred (not ready to publish)", () => {
    expect(
      eligibleForPublishMark(finding({ disposition: { state: "dispute", note: null, by: "you", at: 1 } })),
    ).toBe(false);
    expect(
      eligibleForPublishMark(finding({ disposition: { state: "fix-later", note: null, by: "you", at: 1 } })),
    ).toBe(false);
  });

  it("is NOT eligible once already published, regardless of disposition", () => {
    expect(
      eligibleForPublishMark(finding({ published_state: "published", disposition: null })),
    ).toBe(false);
  });
});
