import { describe, expect, it } from "vitest";
import type { ReviewComment, ReviewCommentsOut } from "../api/types";
import {
  buildAskAgentPayload,
  buildReviewCommentPayload,
  commentSide,
  findThread,
  indexThreads,
  threadLineKey,
} from "./reviewComments";

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

function response(comments: ReviewComment[], path = "src/lib.rs"): ReviewCommentsOut {
  return {
    schema: "review-comments/1",
    review_id: 1,
    repo: "fixture",
    ps: 1,
    groups: [{ path, comments }],
  };
}

describe("threadLineKey", () => {
  it("joins path, side, and line", () => {
    expect(threadLineKey("a/b.rs", "old", 12)).toBe("a/b.rs|old|12");
  });
});

describe("commentSide", () => {
  it("treats missing/unknown as new", () => {
    expect(commentSide({ side: "old" })).toBe("old");
    expect(commentSide({ side: "new" })).toBe("new");
    expect(commentSide({ side: null })).toBe("new");
    expect(commentSide({ side: "other" })).toBe("new");
  });
});

describe("indexThreads", () => {
  it("indexes a live thread under path|side|line", () => {
    const idx = indexThreads(response([comment({ id: "a", side: "new", resolution: {
      line: 7,
      orphaned: false,
      resolved_against: { ps: 1, sha: "x" },
    } })]));
    expect(idx.byLine.get("src/lib.rs|new|7")?.map((c) => c.id)).toEqual(["a"]);
    expect(idx.orphansByPath.size).toBe(0);
    expect(idx.rollup.open).toBe(1);
    expect(idx.rollup.perFile.get("src/lib.rs")).toEqual({ open: 1, total: 1 });
  });

  it("puts orphaned threads on orphansByPath and never on byLine", () => {
    const idx = indexThreads(
      response([
        comment({
          id: "orphan",
          resolution: {
            line: null,
            orphaned: true,
            resolved_against: { ps: 2, sha: "y" },
            original: { ps: 1, side: "new", line: 3, snippet: "fn x" },
          },
        }),
      ]),
    );
    expect(idx.byLine.size).toBe(0);
    expect(idx.orphansByPath.get("src/lib.rs")?.map((c) => c.id)).toEqual(["orphan"]);
  });

  it("never places an orphan on byLine even if a leftover line is present", () => {
    const idx = indexThreads(
      response([
        comment({
          id: "orphan",
          resolution: {
            line: 9,
            orphaned: true,
            resolved_against: { ps: 2, sha: "y" },
          },
        }),
      ]),
    );
    expect(idx.byLine.size).toBe(0);
    expect(idx.orphansByPath.get("src/lib.rs")?.[0].id).toBe("orphan");
  });

  it("counts resolved parents in total but not open", () => {
    const idx = indexThreads(
      response([
        comment({ id: "open", resolved: false }),
        comment({ id: "done", resolved: true, resolution: {
          line: 8,
          orphaned: false,
          resolved_against: { ps: 1, sha: "x" },
        } }),
      ]),
    );
    expect(idx.rollup.open).toBe(1);
    expect(idx.rollup.perFile.get("src/lib.rs")).toEqual({ open: 1, total: 2 });
    expect(idx.byLine.get("src/lib.rs|new|8")?.[0].id).toBe("done");
  });

  it("groups several threads on the same line", () => {
    const idx = indexThreads(
      response([
        comment({ id: "a", resolution: { line: 4, orphaned: false, resolved_against: { ps: 1, sha: "x" } } }),
        comment({ id: "b", resolution: { line: 4, orphaned: false, resolved_against: { ps: 1, sha: "x" } } }),
      ]),
    );
    expect(idx.byLine.get("src/lib.rs|new|4")?.map((c) => c.id)).toEqual(["a", "b"]);
  });

  it("keys old-side threads separately from new-side", () => {
    const idx = indexThreads(
      response([
        comment({ id: "old", side: "old", resolution: { line: 2, orphaned: false, resolved_against: { ps: 1, sha: "x" } } }),
        comment({ id: "new", side: "new", resolution: { line: 2, orphaned: false, resolved_against: { ps: 1, sha: "x" } } }),
      ]),
    );
    expect(idx.byLine.get("src/lib.rs|old|2")?.map((c) => c.id)).toEqual(["old"]);
    expect(idx.byLine.get("src/lib.rs|new|2")?.map((c) => c.id)).toEqual(["new"]);
  });

  it("counts an open orphan in open + total", () => {
    const idx = indexThreads(
      response([
        comment({
          id: "orphan",
          resolved: false,
          resolution: { line: null, orphaned: true, resolved_against: { ps: 2, sha: "y" } },
        }),
      ]),
    );
    expect(idx.rollup.open).toBe(1);
    expect(idx.rollup.perFile.get("src/lib.rs")).toEqual({ open: 1, total: 1 });
  });

  it("skips a non-orphaned comment with no resolvable line", () => {
    const idx = indexThreads(
      response([
        comment({
          id: "hole",
          resolution: { line: null, orphaned: false, resolved_against: { ps: 1, sha: "x" } },
        }),
      ]),
    );
    expect(idx.byLine.size).toBe(0);
    expect(idx.orphansByPath.size).toBe(0);
    expect(idx.rollup.perFile.get("src/lib.rs")?.total).toBe(1);
  });
});

describe("findThread", () => {
  it("returns the parent by id or null", () => {
    const res = response([comment({ id: "hit" }), comment({ id: "other" })]);
    expect(findThread(res, "hit")?.id).toBe("hit");
    expect(findThread(res, "missing")).toBeNull();
  });
});

describe("buildReviewCommentPayload", () => {
  const base = { repo: "fixture", path: "src/lib.rs", line: 5, body: "look", reviewId: 9 };

  it("extends buildCreatePayload with review_id", () => {
    expect(buildReviewCommentPayload(base)).toEqual({
      repo: "fixture",
      path: "src/lib.rs",
      line: 5,
      body: "look",
      review_id: 9,
    });
  });

  it("trims the body and rejects empty/whitespace-only", () => {
    expect(buildReviewCommentPayload({ ...base, body: "  hi  " })?.body).toBe("hi");
    expect(buildReviewCommentPayload({ ...base, body: "" })).toBeNull();
    expect(buildReviewCommentPayload({ ...base, body: "   " })).toBeNull();
  });

  it("rejects a non-positive line", () => {
    expect(buildReviewCommentPayload({ ...base, line: 0 })).toBeNull();
  });

  it("rejects diff and symbol kinds (line|range only)", () => {
    expect(buildReviewCommentPayload({ ...base, anchorKind: "diff", sha: "abc" })).toBeNull();
    expect(buildReviewCommentPayload({ ...base, anchorKind: "symbol" })).toBeNull();
  });

  it("builds a range payload and rejects a missing lineEnd", () => {
    expect(buildReviewCommentPayload({ ...base, lineEnd: 8, anchorKind: "range" })).toEqual({
      repo: "fixture",
      path: "src/lib.rs",
      line: 5,
      line_end: 8,
      anchor_kind: "range",
      body: "look",
      review_id: 9,
    });
    expect(buildReviewCommentPayload({ ...base, anchorKind: "range" })).toBeNull();
  });

  it("omits default side/intent/anchor_kind; includes non-defaults", () => {
    const def = buildReviewCommentPayload({ ...base, side: "new", intent: "note", anchorKind: "line" });
    expect(def).not.toHaveProperty("side");
    expect(def).not.toHaveProperty("intent");
    expect(def).not.toHaveProperty("anchor_kind");

    const flagged = buildReviewCommentPayload({ ...base, side: "old", intent: "question", ps: 2 });
    expect(flagged?.side).toBe("old");
    expect(flagged?.intent).toBe("question");
    expect(flagged?.ps).toBe(2);
  });

  it("omits ps when latest / missing / unparseable", () => {
    expect(buildReviewCommentPayload({ ...base, ps: "latest" })).not.toHaveProperty("ps");
    expect(buildReviewCommentPayload({ ...base, ps: "" })).not.toHaveProperty("ps");
    expect(buildReviewCommentPayload({ ...base, ps: "nope" })).not.toHaveProperty("ps");
  });
});

// ── PRR-U4 (§4 — "the question/answer loop") ──
describe("buildAskAgentPayload", () => {
  it("builds a path-less, anchor_kind:'review', intent:'question' payload", () => {
    expect(buildAskAgentPayload({ repo: "fixture", reviewId: 9, body: "why not X?" })).toEqual({
      repo: "fixture",
      path: "",
      anchor_kind: "review",
      intent: "question",
      review_id: 9,
      body: "why not X?",
    });
  });

  it("trims the body and rejects empty/whitespace-only", () => {
    expect(buildAskAgentPayload({ repo: "fixture", reviewId: 9, body: "  hi  " })?.body).toBe("hi");
    expect(buildAskAgentPayload({ repo: "fixture", reviewId: 9, body: "" })).toBeNull();
    expect(buildAskAgentPayload({ repo: "fixture", reviewId: 9, body: "   " })).toBeNull();
  });

  it("never sets a line/side/ps — the question targets the review, not a patchset blob", () => {
    const payload = buildAskAgentPayload({ repo: "fixture", reviewId: 9, body: "why?" });
    expect(payload).not.toHaveProperty("line");
    expect(payload).not.toHaveProperty("side");
    expect(payload).not.toHaveProperty("ps");
  });
});
