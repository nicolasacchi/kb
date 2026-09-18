import { describe, expect, it } from "vitest";
import { buildTourStops, clampTourStep, tourStopKey, type TourStop } from "./reviewTour";
import type { ReviewFinding } from "../api/types";

function finding(slug: string, path: string, severity: "blocker" | "concern" | "ok", createdAt: number): ReviewFinding {
  return {
    slug,
    severity,
    category: "cat",
    location: { kind: "single", path, lines: [1], removed: false },
    title: `t-${slug}`,
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
    annotation_id: `ann-${slug}`,
    import_batch_id: "batch",
    created_at: createdAt,
    updated_at: createdAt,
    resolution: { line: 1, line_end: null, orphaned: false, confidence: "exact" },
    thread_count: 0,
    unresolved_count: 0,
  };
}

describe("buildTourStops", () => {
  it("emits one plain stop for a file with no findings", () => {
    const stops = buildTourStops(["a.rb", "b.rb"], []);
    expect(stops).toEqual<TourStop[]>([{ path: "a.rb" }, { path: "b.rb" }]);
  });

  it("emits one stop per finding, severity-ordered, and skips the plain file stop", () => {
    const findings = [
      finding("f-ok", "a.rb", "ok", 100),
      finding("f-blocker", "a.rb", "blocker", 200),
      finding("f-concern", "a.rb", "concern", 150),
    ];
    const stops = buildTourStops(["a.rb"], findings);
    expect(stops.map((s) => s.findingSlug)).toEqual(["f-blocker", "f-concern", "f-ok"]);
    expect(stops.every((s) => s.path === "a.rb")).toBe(true);
  });

  it("ties within the same severity break by created_at ascending", () => {
    const findings = [
      finding("f-later", "a.rb", "concern", 200),
      finding("f-earlier", "a.rb", "concern", 100),
    ];
    const stops = buildTourStops(["a.rb"], findings);
    expect(stops.map((s) => s.findingSlug)).toEqual(["f-earlier", "f-later"]);
  });

  it("interleaves findings-having files with plain files in reading order", () => {
    const findings = [finding("f-a", "b.rb", "blocker", 100)];
    const stops = buildTourStops(["a.rb", "b.rb", "c.rb"], findings);
    expect(stops).toEqual<TourStop[]>([
      { path: "a.rb" },
      { path: "b.rb", findingSlug: "f-a", annotationId: "ann-f-a" },
      { path: "c.rb" },
    ]);
  });

  it("is empty for an empty reading order", () => {
    expect(buildTourStops([], [])).toEqual([]);
  });

  // V80-M4 — "the guided tour mentions human threads."
  it("defaults humanThreads to [] — every pre-M4 call site is byte-identical", () => {
    const findings = [finding("f-a", "a.rb", "blocker", 100)];
    expect(buildTourStops(["a.rb"], findings)).toEqual(buildTourStops(["a.rb"], findings, []));
  });

  it("adds one stop per open human thread on a plain (finding-less) file", () => {
    const stops = buildTourStops(
      ["a.rb", "b.rb"],
      [],
      [{ path: "b.rb", id: "ann-1", createdAt: 100 }],
    );
    expect(stops).toEqual<TourStop[]>([{ path: "a.rb" }, { path: "b.rb", annotationId: "ann-1" }]);
  });

  it("orders human-thread stops by createdAt ascending, AFTER the file's finding stops", () => {
    const findings = [finding("f-a", "a.rb", "blocker", 50)];
    const stops = buildTourStops(
      ["a.rb"],
      findings,
      [
        { path: "a.rb", id: "ann-later", createdAt: 200 },
        { path: "a.rb", id: "ann-earlier", createdAt: 100 },
      ],
    );
    expect(stops).toEqual<TourStop[]>([
      { path: "a.rb", findingSlug: "f-a", annotationId: "ann-f-a" },
      { path: "a.rb", annotationId: "ann-earlier" },
      { path: "a.rb", annotationId: "ann-later" },
    ]);
  });

  it("never double-visits a manual finding's own backing annotation as a second thread stop", () => {
    const findings = [finding("f-manual", "a.rb", "concern", 100)];
    const stops = buildTourStops(
      ["a.rb"],
      findings,
      // Same id as the finding's own `annotation_id` (`ann-f-manual`) —
      // the manual finding's thread is human-authored and would otherwise
      // land here too.
      [{ path: "a.rb", id: "ann-f-manual", createdAt: 100 }],
    );
    expect(stops).toEqual<TourStop[]>([{ path: "a.rb", findingSlug: "f-manual", annotationId: "ann-f-manual" }]);
  });

  it("a human thread alone (no findings) still suppresses the plain file stop", () => {
    const stops = buildTourStops(["a.rb"], [], [{ path: "a.rb", id: "ann-1", createdAt: 100 }]);
    expect(stops).toEqual<TourStop[]>([{ path: "a.rb", annotationId: "ann-1" }]);
  });

  it("a human thread on a path outside the reading order is simply never reached", () => {
    const stops = buildTourStops(["a.rb"], [], [{ path: "outside.rb", id: "ann-1", createdAt: 100 }]);
    expect(stops).toEqual<TourStop[]>([{ path: "a.rb" }]);
  });
});

describe("tourStopKey", () => {
  it("keys a plain stop by path alone", () => {
    expect(tourStopKey({ path: "a.rb" })).toBe("a.rb");
  });
  it("keys a finding stop by path#slug", () => {
    expect(tourStopKey({ path: "a.rb", findingSlug: "f-x" })).toBe("a.rb#f-x");
  });
  it("keys a thread-only stop (no findingSlug) by path#annotationId", () => {
    expect(tourStopKey({ path: "a.rb", annotationId: "ann-1" })).toBe("a.rb#ann-1");
  });
});

describe("clampTourStep", () => {
  it("clamps below zero to zero", () => {
    expect(clampTourStep(-1, 5)).toBe(0);
  });
  it("clamps at/above length to the last index", () => {
    expect(clampTourStep(5, 5)).toBe(4);
    expect(clampTourStep(99, 5)).toBe(4);
  });
  it("passes an in-range index through unchanged", () => {
    expect(clampTourStep(2, 5)).toBe(2);
  });
  it("returns null for an empty stop list", () => {
    expect(clampTourStep(0, 0)).toBeNull();
  });
});
