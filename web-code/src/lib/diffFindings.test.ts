import { describe, expect, it } from "vitest";
import type { ReviewFinding } from "../api/types";
import {
  buildManualFindingPayload,
  countByOverlay,
  dispositionHint,
  dispositionLabel,
  findingDispositionChips,
  findingHeaderView,
  findingsByAnnotationId,
  findingsBySlug,
  githubThreadVisibleInOverlay,
  isFindingDisposition,
  isFindingSeverity,
  nextOverlayMode,
  overlayParamValue,
  parseOverlayParam,
  severityLabel,
  severityRank,
  threadVisibleInOverlay,
  worstSeverity,
} from "./diffFindings";

function finding(overrides: Partial<ReviewFinding> = {}): ReviewFinding {
  return {
    slug: "f-dedup-race",
    severity: "concern",
    category: "correctness",
    location: { kind: "single", path: "src/lib.rs", lines: [10], removed: false },
    title: "Duplicate order rows",
    rationale: "Two writers can race.",
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
    content_updated_at: 1000,
    annotation_id: "ann-1",
    import_batch_id: "batch_abc",
    created_at: 1000,
    updated_at: 1000,
    resolution: { line: 10, line_end: null, orphaned: false, confidence: "exact" },
    thread_count: 0,
    unresolved_count: 0,
    ...overrides,
  };
}

describe("isFindingSeverity / isFindingDisposition", () => {
  it("accepts exactly the plan-arbitration vocab (not design-ui.md's draft 5-value table)", () => {
    expect(isFindingSeverity("blocker")).toBe(true);
    expect(isFindingSeverity("concern")).toBe(true);
    expect(isFindingSeverity("ok")).toBe(true);
    expect(isFindingSeverity("nit")).toBe(false);
    expect(isFindingSeverity("praise")).toBe(false);
    expect(isFindingSeverity("info")).toBe(false);
  });

  it("disposition uses the REAL verb names — fix-later, not fixed/follow-up", () => {
    expect(isFindingDisposition("agree")).toBe(true);
    expect(isFindingDisposition("dispute")).toBe(true);
    expect(isFindingDisposition("waive")).toBe(true);
    expect(isFindingDisposition("fix-later")).toBe(true);
    expect(isFindingDisposition("fixed")).toBe(false);
    expect(isFindingDisposition("follow-up")).toBe(false);
  });
});

describe("severityRank", () => {
  it("orders blocker < concern < ok", () => {
    expect(severityRank("blocker")).toBeLessThan(severityRank("concern"));
    expect(severityRank("concern")).toBeLessThan(severityRank("ok"));
  });

  it("sorts an unrecognized/missing severity last, never throws", () => {
    expect(severityRank("bogus")).toBeGreaterThan(severityRank("ok"));
    expect(severityRank(null)).toBeGreaterThan(severityRank("ok"));
    expect(severityRank(undefined)).toBeGreaterThan(severityRank("ok"));
  });
});

describe("severityLabel / dispositionLabel / dispositionHint", () => {
  it("labels the closed vocab", () => {
    expect(severityLabel("blocker")).toBe("Blocker");
    expect(severityLabel("concern")).toBe("Concern");
    expect(severityLabel("ok")).toBe("OK");
    expect(dispositionLabel("fix-later")).toBe("Fix later");
  });

  it("degrades an unrecognized value to itself verbatim", () => {
    expect(severityLabel("weird")).toBe("weird");
    expect(dispositionLabel("weird")).toBe("weird");
  });

  it("every disposition has a non-empty hint", () => {
    for (const d of ["agree", "dispute", "waive", "fix-later"]) {
      expect(dispositionHint(d)).not.toBe("");
    }
    expect(dispositionHint("bogus")).toBe("");
  });
});

describe("findingsByAnnotationId / findingsBySlug", () => {
  it("indexes by annotation_id and by slug", () => {
    const a = finding({ slug: "f-a", annotation_id: "ann-a" });
    const b = finding({ slug: "f-b", annotation_id: "ann-b" });
    const byAnn = findingsByAnnotationId([a, b]);
    expect(byAnn.get("ann-a")?.slug).toBe("f-a");
    expect(byAnn.get("ann-b")?.slug).toBe("f-b");
    expect(byAnn.size).toBe(2);

    const bySlug = findingsBySlug([a, b]);
    expect(bySlug.get("f-a")?.annotation_id).toBe("ann-a");
    expect(bySlug.size).toBe(2);
  });

  it("is total over an empty list", () => {
    expect(findingsByAnnotationId([]).size).toBe(0);
    expect(findingsBySlug([]).size).toBe(0);
  });
});

describe("parseOverlayParam / overlayParamValue / nextOverlayMode", () => {
  it("parses the closed set, defaulting anything else to 'all'", () => {
    expect(parseOverlayParam("findings")).toBe("findings");
    expect(parseOverlayParam("comments")).toBe("comments");
    expect(parseOverlayParam("diagnostics")).toBe("diagnostics");
    expect(parseOverlayParam("none")).toBe("none");
    expect(parseOverlayParam("all")).toBe("all");
    expect(parseOverlayParam("bogus")).toBe("all");
    expect(parseOverlayParam(null)).toBe("all");
    expect(parseOverlayParam(undefined)).toBe("all");
  });

  it("omits the default 'all' from the query value; writes the rest verbatim", () => {
    expect(overlayParamValue("all")).toBeUndefined();
    expect(overlayParamValue("findings")).toBe("findings");
    expect(overlayParamValue("comments")).toBe("comments");
    expect(overlayParamValue("diagnostics")).toBe("diagnostics");
    expect(overlayParamValue("none")).toBe("none");
  });

  // PRR-U9 — extended the cycle with a "diagnostics" stop between
  // "comments" and "none" (design-addendum-2.md §D: "the overlay selector
  // gains a 'diagnostics' lane"). PRR-F — extended again with a "github"
  // stop between "diagnostics" and "none" (design-addendum-2.md §A).
  it("cycles all -> findings -> comments -> diagnostics -> github -> none -> all", () => {
    expect(nextOverlayMode("all")).toBe("findings");
    expect(nextOverlayMode("findings")).toBe("comments");
    expect(nextOverlayMode("comments")).toBe("diagnostics");
    expect(nextOverlayMode("diagnostics")).toBe("github");
    expect(nextOverlayMode("github")).toBe("none");
    expect(nextOverlayMode("none")).toBe("all");
  });
});

describe("threadVisibleInOverlay", () => {
  it("all shows everything; none hides everything", () => {
    expect(threadVisibleInOverlay(true, "all")).toBe(true);
    expect(threadVisibleInOverlay(false, "all")).toBe(true);
    expect(threadVisibleInOverlay(true, "none")).toBe(false);
    expect(threadVisibleInOverlay(false, "none")).toBe(false);
  });

  it("findings shows only findings; comments shows only non-findings", () => {
    expect(threadVisibleInOverlay(true, "findings")).toBe(true);
    expect(threadVisibleInOverlay(false, "findings")).toBe(false);
    expect(threadVisibleInOverlay(true, "comments")).toBe(false);
    expect(threadVisibleInOverlay(false, "comments")).toBe(true);
  });

  // PRR-U9 — diagnostics is its own lane: hides every thread, same as
  // "none", never an additive filter alongside findings/comments.
  it("diagnostics hides everything, same as none", () => {
    expect(threadVisibleInOverlay(true, "diagnostics")).toBe(false);
    expect(threadVisibleInOverlay(false, "diagnostics")).toBe(false);
  });

  // PRR-F — github is likewise its own exclusive lane over LOCAL threads.
  it("github hides every local thread, same as none/diagnostics", () => {
    expect(threadVisibleInOverlay(true, "github")).toBe(false);
    expect(threadVisibleInOverlay(false, "github")).toBe(false);
  });
});

describe("githubThreadVisibleInOverlay", () => {
  it("is true ONLY in the github lane, never all/findings/comments/diagnostics/none", () => {
    expect(githubThreadVisibleInOverlay("github")).toBe(true);
    expect(githubThreadVisibleInOverlay("all")).toBe(false);
    expect(githubThreadVisibleInOverlay("findings")).toBe(false);
    expect(githubThreadVisibleInOverlay("comments")).toBe(false);
    expect(githubThreadVisibleInOverlay("diagnostics")).toBe(false);
    expect(githubThreadVisibleInOverlay("none")).toBe(false);
  });
});

describe("parseOverlayParam (github)", () => {
  it("accepts github and rejects anything unrecognized to all", () => {
    expect(parseOverlayParam("github")).toBe("github");
    expect(parseOverlayParam("bogus")).toBe("all");
  });
});

describe("countByOverlay", () => {
  it("splits thread ids into findings vs comments, unfiltered by overlay", () => {
    const map = findingsByAnnotationId([finding({ annotation_id: "f1" }), finding({ annotation_id: "f2" })]);
    const counts = countByOverlay(["f1", "f2", "c1", "c2", "c3"], map);
    expect(counts).toEqual({ findings: 2, comments: 3 });
  });

  it("is zero/zero for an empty thread list", () => {
    expect(countByOverlay([], new Map())).toEqual({ findings: 0, comments: 0 });
  });
});

describe("worstSeverity", () => {
  it("picks the most severe among several findings", () => {
    expect(
      worstSeverity([finding({ severity: "ok" }), finding({ severity: "blocker" }), finding({ severity: "concern" })]),
    ).toBe("blocker");
  });

  it("returns null for an empty list", () => {
    expect(worstSeverity([])).toBeNull();
  });

  it("ignores an unrecognized severity value rather than crashing", () => {
    expect(worstSeverity([finding({ severity: "bogus" as never }), finding({ severity: "concern" })])).toBe("concern");
    expect(worstSeverity([finding({ severity: "bogus" as never })])).toBeNull();
  });
});

describe("findingHeaderView", () => {
  it("marks import origin as agentMark=true", () => {
    const v = findingHeaderView(finding({ origin: "import", author: "claude" }));
    expect(v.agentMark).toBe(true);
    expect(v.author).toBe("claude");
  });

  it("marks manual origin as agentMark=false ('you')", () => {
    const v = findingHeaderView(finding({ origin: "manual", author: "you" }));
    expect(v.agentMark).toBe(false);
    expect(v.author).toBe("you");
  });

  it("carries severity label, category, and slug", () => {
    const v = findingHeaderView(finding({ severity: "blocker", category: "security", slug: "f-x" }));
    expect(v.severityLabel).toBe("Blocker");
    expect(v.category).toBe("security");
    expect(v.slug).toBe("f-x");
  });
});

describe("findingDispositionChips", () => {
  it("returns all four chips, none active when disposition is unset", () => {
    const chips = findingDispositionChips(finding({ disposition: null }));
    expect(chips.map((c) => c.value)).toEqual(["agree", "dispute", "waive", "fix-later"]);
    expect(chips.every((c) => !c.active)).toBe(true);
  });

  it("marks exactly the current disposition active", () => {
    const chips = findingDispositionChips(
      finding({ disposition: { state: "waive", note: null, by: "you", at: 5 } }),
    );
    expect(chips.find((c) => c.value === "waive")?.active).toBe(true);
    expect(chips.filter((c) => c.active)).toHaveLength(1);
  });
});

describe("buildManualFindingPayload", () => {
  const base = {
    path: "src/lib.rs",
    side: "new" as const,
    line: 10,
    severity: "blocker",
    category: "correctness",
    title: "Duplicate order rows",
    rationale: "Two writers race.",
  };

  it("builds a single-line, side-aware location", () => {
    expect(buildManualFindingPayload(base)).toEqual({
      severity: "blocker",
      category: "correctness",
      location: { kind: "single", path: "src/lib.rs", lines: [10], removed: false },
      title: "Duplicate order rows",
      rationale: "Two writers race.",
    });
  });

  it("side 'old' maps to location.removed: true", () => {
    expect(buildManualFindingPayload({ ...base, side: "old" })?.location.removed).toBe(true);
  });

  it("includes recommendation only when given (trimmed)", () => {
    expect(buildManualFindingPayload(base)).not.toHaveProperty("recommendation");
    expect(buildManualFindingPayload({ ...base, recommendation: "  do X  " })?.recommendation).toBe("do X");
  });

  it("trims severity/category/title/rationale", () => {
    const out = buildManualFindingPayload({
      ...base,
      severity: " blocker ",
      category: "  correctness  ",
      title: "  T  ",
      rationale: "  R  ",
    });
    expect(out?.severity).toBe("blocker");
    expect(out?.category).toBe("correctness");
    expect(out?.title).toBe("T");
    expect(out?.rationale).toBe("R");
  });

  it("rejects an invalid severity", () => {
    expect(buildManualFindingPayload({ ...base, severity: "nit" })).toBeNull();
    expect(buildManualFindingPayload({ ...base, severity: "" })).toBeNull();
  });

  it("rejects empty category/title/rationale", () => {
    expect(buildManualFindingPayload({ ...base, category: "  " })).toBeNull();
    expect(buildManualFindingPayload({ ...base, title: "" })).toBeNull();
    expect(buildManualFindingPayload({ ...base, rationale: "   " })).toBeNull();
  });

  it("rejects a non-positive/non-finite line", () => {
    expect(buildManualFindingPayload({ ...base, line: 0 })).toBeNull();
    expect(buildManualFindingPayload({ ...base, line: -1 })).toBeNull();
    expect(buildManualFindingPayload({ ...base, line: Number.NaN })).toBeNull();
  });
});
