import { describe, expect, it } from "vitest";
import type { ReviewComment, ReviewCommentsOut } from "../api/types";
import {
  failingVerdicts,
  summarizeBatchVerdicts,
  unappliedSuggestionById,
  unappliedSuggestionRows,
} from "./suggestionsBatch";

function comment(overrides: Partial<ReviewComment> = {}): ReviewComment {
  return {
    id: "ann1",
    path: "a.rb",
    intent: "note",
    body: "hi",
    author: "you",
    created_at: 1,
    updated_at: 1,
    resolved: false,
    anchor_kind: "line",
    side: "new",
    ps_number: 1,
    resolution: { line: 10, line_end: null, orphaned: false, resolved_against: { ps: 1, sha: "sha1" } },
    suggestion: null,
    replies: [],
    ...overrides,
  };
}

function out(groups: { path: string; comments: ReviewComment[] }[]): ReviewCommentsOut {
  return { schema: "review-comments/1", review_id: 1, repo: "r", ps: 1, groups };
}

describe("unappliedSuggestionRows", () => {
  it("is empty when no comment carries a suggestion", () => {
    const resp = out([{ path: "a.rb", comments: [comment()] }]);
    expect(unappliedSuggestionRows(resp)).toEqual([]);
  });

  it("skips a suggestion that is already applied", () => {
    const resp = out([
      {
        path: "a.rb",
        comments: [
          comment({ suggestion: { replacement: "x", original: "y", applied: true, applied_at: 5 } }),
        ],
      },
    ]);
    expect(unappliedSuggestionRows(resp)).toEqual([]);
  });

  it("includes an unapplied suggestion with a truncated first-line preview", () => {
    const resp = out([
      {
        path: "app/models/order.rb",
        comments: [
          comment({
            id: "ann9",
            resolution: { line: 42, line_end: null, orphaned: false, resolved_against: { ps: 1, sha: "sha1" } },
            suggestion: { replacement: "def foo\n  bar\nend", original: "def foo\nend", applied: false, applied_at: null },
          }),
        ],
      },
    ]);
    const rows = unappliedSuggestionRows(resp);
    expect(rows).toEqual([
      { id: "ann9", path: "app/models/order.rb", line: 42, preview: "def foo", orphaned: false },
    ]);
  });

  it("truncates a long first line at 60 chars with an ellipsis", () => {
    const long = "x".repeat(80);
    const resp = out([
      {
        path: "a.rb",
        comments: [comment({ suggestion: { replacement: long, original: "y", applied: false, applied_at: null } })],
      },
    ]);
    const preview = unappliedSuggestionRows(resp)[0].preview;
    expect(preview.length).toBe(61); // 60 chars + ellipsis
    expect(preview.endsWith("…")).toBe(true);
  });

  it("flags an orphaned thread's suggestion as orphaned (still listed, not dropped)", () => {
    const resp = out([
      {
        path: "a.rb",
        comments: [
          comment({
            resolution: { line: null, line_end: null, orphaned: true, resolved_against: { ps: 1, sha: "sha1" } },
            suggestion: { replacement: "x", original: "y", applied: false, applied_at: null },
          }),
        ],
      },
    ]);
    expect(unappliedSuggestionRows(resp)[0].orphaned).toBe(true);
  });

  it("flattens across multiple path groups", () => {
    const s = { replacement: "x", original: "y", applied: false, applied_at: null };
    const resp = out([
      { path: "a.rb", comments: [comment({ id: "a1", suggestion: s })] },
      { path: "b.rb", comments: [comment({ id: "b1", suggestion: s })] },
    ]);
    expect(unappliedSuggestionRows(resp).map((r) => r.id)).toEqual(["a1", "b1"]);
  });
});

describe("unappliedSuggestionById", () => {
  it("maps only unapplied-suggestion comments by id", () => {
    const s = { replacement: "x", original: "y", applied: false, applied_at: null };
    const resp = out([
      {
        path: "a.rb",
        comments: [
          comment({ id: "keep", suggestion: s }),
          comment({ id: "no-suggestion", suggestion: null }),
          comment({ id: "already-applied", suggestion: { ...s, applied: true } }),
        ],
      },
    ]);
    const byId = unappliedSuggestionById(resp);
    expect(Array.from(byId.keys())).toEqual(["keep"]);
  });
});

describe("summarizeBatchVerdicts", () => {
  it("counts ok vs error and buckets by error kind", () => {
    const summary = summarizeBatchVerdicts([
      { id: "a", ok: true },
      { id: "b", ok: false, error: { kind: "drift", detail: "d1" } },
      { id: "c", ok: false, error: { kind: "drift", detail: "d2" } },
      { id: "d", ok: false, error: { kind: "overlap", detail: "d3" } },
    ]);
    expect(summary).toEqual({ okCount: 1, errCount: 3, errorsByKind: { drift: 2, overlap: 1 } });
  });

  it("buckets a missing error kind under 'unknown' rather than crashing", () => {
    const summary = summarizeBatchVerdicts([{ id: "a", ok: false }]);
    expect(summary.errorsByKind).toEqual({ unknown: 1 });
  });

  it("is all-zero on an empty verdict list", () => {
    expect(summarizeBatchVerdicts([])).toEqual({ okCount: 0, errCount: 0, errorsByKind: {} });
  });
});

describe("failingVerdicts", () => {
  it("keeps only ok:false entries, preserving order", () => {
    const verdicts = [
      { id: "a", ok: true },
      { id: "b", ok: false, error: { kind: "drift", detail: "d" } },
      { id: "c", ok: true },
      { id: "d", ok: false, error: { kind: "overlap", detail: "d" } },
    ];
    expect(failingVerdicts(verdicts).map((v) => v.id)).toEqual(["b", "d"]);
  });
});
