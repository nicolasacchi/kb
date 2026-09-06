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
});

describe("tourStopKey", () => {
  it("keys a plain stop by path alone", () => {
    expect(tourStopKey({ path: "a.rb" })).toBe("a.rb");
  });
  it("keys a finding stop by path#slug", () => {
    expect(tourStopKey({ path: "a.rb", findingSlug: "f-x" })).toBe("a.rb#f-x");
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
