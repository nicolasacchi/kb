import { describe, expect, it } from "vitest";
import { buildWorkOrder, disputedFindings, openQuestions, verdictsDisagree } from "./dialectic";
import type { ReviewComment, ReviewCommentsOut, ReviewFinding } from "../api/types";

describe("verdictsDisagree", () => {
  it("is false when either side is unset", () => {
    expect(verdictsDisagree(null, "approve")).toBe(false);
    expect(verdictsDisagree("blocker", undefined)).toBe(false);
  });
  it("is false when the agent hedges with concern", () => {
    expect(verdictsDisagree("concern", "approve")).toBe(false);
    expect(verdictsDisagree("concern", "request-changes")).toBe(false);
  });
  it("is false when the human hedges with comment", () => {
    expect(verdictsDisagree("blocker", "comment")).toBe(false);
    expect(verdictsDisagree("ok", "comment")).toBe(false);
  });
  it("is true for a blocker vs an approve", () => {
    expect(verdictsDisagree("blocker", "approve")).toBe(true);
  });
  it("is true for an ok vs a request-changes", () => {
    expect(verdictsDisagree("ok", "request-changes")).toBe(true);
  });
  it("is false when both sides land on the same polarity", () => {
    expect(verdictsDisagree("blocker", "request-changes")).toBe(false);
    expect(verdictsDisagree("ok", "approve")).toBe(false);
  });
});

function finding(overrides: Partial<ReviewFinding> = {}): ReviewFinding {
  return {
    slug: "f-a",
    severity: "blocker",
    category: "Security",
    location: { kind: "single", path: "a.rb", lines: [1], removed: false },
    title: "leaking token",
    rationale: "the token is logged in plaintext",
    recommendation: null,
    evidence: null,
    origin: "import",
    author: "claude",
    disposition: { state: "dispute", note: "this is intentional for audit logs", by: "you", at: 100 },
    published_state: "unpublished",
    published_at: null,
    published_url: null,
    superseded: false,
    superseded_reason: null,
    content_updated_at: null,
    annotation_id: "ann-1",
    import_batch_id: "batch",
    created_at: 1,
    updated_at: 1,
    resolution: { line: 1, line_end: null, orphaned: false, confidence: "exact" },
    thread_count: 0,
    unresolved_count: 0,
    ...overrides,
  };
}

describe("disputedFindings", () => {
  it("includes only non-superseded dispute-disposition findings", () => {
    const findings = [
      finding({ slug: "f-dispute" }),
      finding({ slug: "f-agree", disposition: { state: "agree", note: null, by: "you", at: 1 } }),
      finding({ slug: "f-superseded", superseded: true }),
    ];
    const out = disputedFindings("r", 1, findings);
    expect(out.map((d) => d.slug)).toEqual(["f-dispute"]);
  });

  it("carries the agent rationale and the human's dispute note", () => {
    const out = disputedFindings("r", 1, [finding()]);
    expect(out[0].agentWord).toBe("the token is logged in plaintext");
    expect(out[0].humanWord).toBe("this is intentional for audit logs");
  });

  it("degrades to null (never a fabricated string) when no note was left", () => {
    const out = disputedFindings(
      "r",
      1,
      [finding({ disposition: { state: "dispute", note: null, by: "you", at: 1 } })],
    );
    expect(out[0].humanWord).toBeNull();
  });
});

function comment(overrides: Partial<ReviewComment> = {}): ReviewComment {
  return {
    id: "c-1",
    path: "a.rb",
    intent: "question",
    body: "is this safe?",
    author: "you",
    created_at: 100,
    updated_at: 100,
    resolved: false,
    anchor_kind: "line",
    side: null,
    ps_number: 1,
    replies: [],
    resolution: { line: 1, orphaned: false, resolved_against: { ps: 1, sha: "abc" } },
    suggestion: null,
    ...overrides,
  };
}

describe("openQuestions", () => {
  it("includes only awaiting-agent question threads", () => {
    const data: ReviewCommentsOut = {
      schema: "s",
      review_id: 1,
      repo: "r",
      ps: 1,
      groups: [{ path: "a.rb", comments: [comment(), comment({ id: "c-2", intent: "note" })] }],
    };
    const out = openQuestions("r", 1, data, new Map());
    expect(out.map((q) => q.id)).toEqual(["c-1"]);
  });

  it("excludes a question already answered by the agent", () => {
    const answered = comment({
      replies: [
        {
          id: "r-1",
          parent_id: "c-1",
          path: "a.rb",
          intent: "question",
          author: "claude",
          body: "yes",
          created_at: 200,
          updated_at: 200,
          resolved: false,
        },
      ],
    });
    const data: ReviewCommentsOut = {
      schema: "s",
      review_id: 1,
      repo: "r",
      ps: 1,
      groups: [{ path: "a.rb", comments: [answered] }],
    };
    expect(openQuestions("r", 1, data, new Map())).toHaveLength(0);
  });
});

describe("buildWorkOrder", () => {
  it("names both sections and every item's link", () => {
    const disputed = disputedFindings("r", 1, [finding()]);
    const questions = openQuestions("r", 1, {
      schema: "s",
      review_id: 1,
      repo: "r",
      ps: 1,
      groups: [{ path: "a.rb", comments: [comment()] }],
    }, new Map());
    const text = buildWorkOrder("https://kb.example", disputed, questions);
    expect(text).toContain("Disputed findings (1)");
    expect(text).toContain("f-a — leaking token");
    expect(text).toContain("Open questions (1)");
    expect(text).toContain("https://kb.example");
  });

  it("says nothing outstanding when both lists are empty", () => {
    expect(buildWorkOrder("https://x", [], [])).toContain("Nothing outstanding");
  });
});
