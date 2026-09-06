import { describe, expect, it } from "vitest";
import type { ReviewFinding, ReviewReportOut } from "../../api/types";
import {
  hasReviewReport,
  importCliCommands,
  liveFindingCounts,
  statDriftCaption,
} from "./ReportPanel";

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

describe("hasReviewReport", () => {
  it("is false for the literal {report: null} no-report shape", () => {
    expect(hasReviewReport({ report: null })).toBe(false);
  });

  it("is false for undefined (query not yet resolved)", () => {
    expect(hasReviewReport(undefined)).toBe(false);
  });

  it("is true for a raw stored report blob with no 'report' key at all", () => {
    const out: ReviewReportOut = { schema: "kbc-review-report/1", summary: "x" };
    expect(hasReviewReport(out)).toBe(true);
  });

  it("is true even for an empty object (a degenerate but still-authored report)", () => {
    expect(hasReviewReport({} as ReviewReportOut)).toBe(true);
  });
});

describe("liveFindingCounts", () => {
  it("counts by severity, excluding superseded findings", () => {
    const counts = liveFindingCounts([
      finding({ severity: "blocker" }),
      finding({ severity: "concern" }),
      finding({ severity: "concern" }),
      finding({ severity: "ok" }),
      finding({ severity: "blocker", superseded: true }),
    ]);
    expect(counts).toEqual({ blockers: 1, concerns: 2, verified: 1 });
  });

  it("is all-zero for an empty finding list", () => {
    expect(liveFindingCounts([])).toEqual({ blockers: 0, concerns: 0, verified: 0 });
  });
});

describe("statDriftCaption", () => {
  it("is null when the report claims no stats at all", () => {
    expect(statDriftCaption(undefined, { blockers: 1, concerns: 0, verified: 0 })).toBeNull();
  });

  it("is null when every claimed count matches live", () => {
    expect(
      statDriftCaption({ blockers: 1, concerns: 2, verified: 3 }, { blockers: 1, concerns: 2, verified: 3 }),
    ).toBeNull();
  });

  it("names a single mismatched count", () => {
    const caption = statDriftCaption({ concerns: 3 }, { blockers: 0, concerns: 2, verified: 0 });
    expect(caption).toContain("3 concerns claimed; 2 live");
  });

  it("names every mismatched count when several disagree", () => {
    const caption = statDriftCaption(
      { blockers: 0, concerns: 3, verified: 6 },
      { blockers: 1, concerns: 2, verified: 6 },
    );
    expect(caption).toContain("blocker");
    expect(caption).toContain("concern");
    expect(caption).not.toContain("verified"); // that one matched
  });

  it("singularizes a claimed count of 1", () => {
    const caption = statDriftCaption({ blockers: 1 }, { blockers: 0, concerns: 0, verified: 0 });
    expect(caption).toContain("1 blocker claimed");
    expect(caption).not.toContain("1 blockers claimed");
  });

  it("ignores a stats key the report never claimed (undefined, not 0)", () => {
    expect(statDriftCaption({ blockers: 1 }, { blockers: 1, concerns: 99, verified: 99 })).toBeNull();
  });
});

describe("importCliCommands", () => {
  it("emits the two-step findings-import + report-set verbs, naming the review id", () => {
    const cmds = importCliCommands(42);
    expect(cmds[0]).toBe("kb-code review findings import 42 --stdin --json");
    expect(cmds[1]).toBe("kb-code review report 42 --set --from-file report.json --json");
  });
});
