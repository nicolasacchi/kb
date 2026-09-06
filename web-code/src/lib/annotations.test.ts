import { describe, expect, it } from "vitest";
import type { AnnotationView } from "../api/types";
import {
  anchorBadgeLabel,
  annotationGutterTitle,
  annotationsByLine,
  buildCreatePayload,
  buildReplyPayload,
  groupThreads,
  intentLabel,
  sortAnnotations,
  unresolvedCount,
} from "./annotations";

function annotation(overrides: Partial<AnnotationView> = {}): AnnotationView {
  return {
    id: "ann-1",
    repo: "fixture",
    path: "src/lib.rs",
    anchor: {},
    anchor_kind: "line",
    intent: "note",
    parent_id: null,
    body: "note",
    author: "you",
    created_at: 1000,
    updated_at: 1000,
    resolved: false,
    line: 1,
    stale: false,
    line_end: null,
    sha: null,
    ...overrides,
  };
}

describe("buildCreatePayload", () => {
  it("builds the POST body when body and line are valid (line kind, the default)", () => {
    expect(buildCreatePayload({ repo: "fixture", path: "src/lib.rs", line: 5, body: "worth a look" })).toEqual({
      repo: "fixture",
      path: "src/lib.rs",
      line: 5,
      body: "worth a look",
    });
  });

  it("trims the body before sending", () => {
    expect(
      buildCreatePayload({ repo: "fixture", path: "src/lib.rs", line: 5, body: "  worth a look  " })?.body,
    ).toBe("worth a look");
  });

  it("includes author only when given", () => {
    expect(
      buildCreatePayload({ repo: "fixture", path: "src/lib.rs", line: 5, body: "note", author: "claude" })?.author,
    ).toBe("claude");
    expect(buildCreatePayload({ repo: "fixture", path: "src/lib.rs", line: 5, body: "note" })).not.toHaveProperty(
      "author",
    );
  });

  it("rejects an empty or whitespace-only body", () => {
    expect(buildCreatePayload({ repo: "fixture", path: "src/lib.rs", line: 5, body: "" })).toBeNull();
    expect(buildCreatePayload({ repo: "fixture", path: "src/lib.rs", line: 5, body: "   " })).toBeNull();
  });

  it("rejects a non-positive line", () => {
    expect(buildCreatePayload({ repo: "fixture", path: "src/lib.rs", line: 0, body: "note" })).toBeNull();
    expect(buildCreatePayload({ repo: "fixture", path: "src/lib.rs", line: -1, body: "note" })).toBeNull();
  });

  it("omits anchor_kind/intent when they're the defaults, includes them otherwise", () => {
    const line = buildCreatePayload({ repo: "r", path: "p", line: 1, body: "b", anchorKind: "line", intent: "note" });
    expect(line).not.toHaveProperty("anchor_kind");
    expect(line).not.toHaveProperty("intent");

    const flagged = buildCreatePayload({ repo: "r", path: "p", line: 1, body: "b", intent: "flag-for-agent" });
    expect(flagged?.intent).toBe("flag-for-agent");
  });

  it("builds a range payload with line_end, rejecting a missing/non-positive one", () => {
    const range = buildCreatePayload({ repo: "r", path: "p", line: 2, lineEnd: 8, body: "range", anchorKind: "range" });
    expect(range).toEqual({ repo: "r", path: "p", line: 2, line_end: 8, anchor_kind: "range", body: "range" });

    expect(buildCreatePayload({ repo: "r", path: "p", line: 2, body: "range", anchorKind: "range" })).toBeNull();
    expect(
      buildCreatePayload({ repo: "r", path: "p", line: 2, lineEnd: 0, body: "range", anchorKind: "range" }),
    ).toBeNull();
  });

  it("builds a symbol payload with no line_end/sha", () => {
    expect(buildCreatePayload({ repo: "r", path: "p", line: 4, body: "sym", anchorKind: "symbol" })).toEqual({
      repo: "r",
      path: "p",
      line: 4,
      anchor_kind: "symbol",
      body: "sym",
    });
  });

  it("builds a diff payload with sha, rejecting a missing one", () => {
    expect(buildCreatePayload({ repo: "r", path: "p", line: 3, body: "d", anchorKind: "diff", sha: "abc123" })).toEqual({
      repo: "r",
      path: "p",
      line: 3,
      anchor_kind: "diff",
      sha: "abc123",
      body: "d",
    });
    expect(buildCreatePayload({ repo: "r", path: "p", line: 3, body: "d", anchorKind: "diff" })).toBeNull();
  });
});

describe("buildReplyPayload", () => {
  it("builds a parent_id-only body", () => {
    expect(buildReplyPayload("r", "p", "ann_parent", "a reply")).toEqual({
      repo: "r",
      path: "p",
      parent_id: "ann_parent",
      body: "a reply",
    });
  });

  it("trims the body and rejects empty/whitespace-only", () => {
    expect(buildReplyPayload("r", "p", "ann_parent", "  hi  ")?.body).toBe("hi");
    expect(buildReplyPayload("r", "p", "ann_parent", "")).toBeNull();
    expect(buildReplyPayload("r", "p", "ann_parent", "   ")).toBeNull();
  });
});

describe("sortAnnotations", () => {
  it("sorts by line ascending", () => {
    const sorted = sortAnnotations([annotation({ id: "b", line: 9 }), annotation({ id: "a", line: 2 })]);
    expect(sorted.map((a) => a.id)).toEqual(["a", "b"]);
  });

  it("breaks ties by created_at ascending", () => {
    const sorted = sortAnnotations([
      annotation({ id: "later", line: 3, created_at: 200 }),
      annotation({ id: "earlier", line: 3, created_at: 100 }),
    ]);
    expect(sorted.map((a) => a.id)).toEqual(["earlier", "later"]);
  });

  it("does not mutate the input array", () => {
    const input = [annotation({ id: "b", line: 9 }), annotation({ id: "a", line: 2 })];
    const copy = [...input];
    sortAnnotations(input);
    expect(input).toEqual(copy);
  });
});

describe("groupThreads", () => {
  it("groups replies under their parent, oldest reply first", () => {
    const parent = annotation({ id: "p1", line: 2 });
    const replyLate = annotation({ id: "r2", parent_id: "p1", created_at: 200, line: 2 });
    const replyEarly = annotation({ id: "r1", parent_id: "p1", created_at: 100, line: 2 });
    const threads = groupThreads([parent, replyLate, replyEarly]);
    expect(threads).toHaveLength(1);
    expect(threads[0].parent.id).toBe("p1");
    expect(threads[0].replies.map((r) => r.id)).toEqual(["r1", "r2"]);
  });

  it("orders threads the same way sortAnnotations orders a flat list", () => {
    const a = annotation({ id: "a", line: 9 });
    const b = annotation({ id: "b", line: 2 });
    const threads = groupThreads([a, b]);
    expect(threads.map((t) => t.parent.id)).toEqual(["b", "a"]);
  });

  it("gives a thread with no replies an empty replies array", () => {
    const threads = groupThreads([annotation({ id: "solo" })]);
    expect(threads[0].replies).toEqual([]);
  });
});

describe("annotationsByLine", () => {
  it("groups multiple annotations on the same line", () => {
    const map = annotationsByLine([annotation({ id: "a", line: 5 }), annotation({ id: "b", line: 5 })]);
    expect(map.get(5)?.map((a) => a.id)).toEqual(["a", "b"]);
  });

  it("keys distinct lines separately", () => {
    const map = annotationsByLine([annotation({ id: "a", line: 5 }), annotation({ id: "b", line: 9 })]);
    expect([...map.keys()].sort((x, y) => x - y)).toEqual([5, 9]);
  });

  it("excludes replies (they'd double-count their already-counted parent's line)", () => {
    const parent = annotation({ id: "p", line: 5 });
    const reply = annotation({ id: "r", parent_id: "p", line: 5 });
    const map = annotationsByLine([parent, reply]);
    expect(map.get(5)?.map((a) => a.id)).toEqual(["p"]);
  });
});

describe("annotationGutterTitle", () => {
  it("shows the body for a single line-kind annotation", () => {
    expect(annotationGutterTitle([annotation({ body: "hello" })])).toBe("hello");
  });

  it("prefixes a range's title with its L{start}–{end} span", () => {
    const title = annotationGutterTitle([annotation({ anchor_kind: "range", line: 10, line_end: 24, body: "why" })]);
    expect(title).toBe("L10–24: why");
  });

  it("collapses to a count when several threads share a line", () => {
    expect(annotationGutterTitle([annotation({ id: "a" }), annotation({ id: "b" })])).toBe("2 annotations");
  });
});

describe("unresolvedCount", () => {
  it("counts only unresolved TOP-LEVEL annotations", () => {
    const count = unresolvedCount([
      annotation({ resolved: false }),
      annotation({ resolved: true }),
      annotation({ resolved: false }),
    ]);
    expect(count).toBe(2);
  });

  it("excludes replies even when unresolved", () => {
    const count = unresolvedCount([
      annotation({ id: "p", resolved: false }),
      annotation({ id: "r", parent_id: "p", resolved: false }),
    ]);
    expect(count).toBe(1);
  });

  it("is zero for an empty list", () => {
    expect(unresolvedCount([])).toBe(0);
  });
});

describe("intentLabel", () => {
  it("labels every known intent", () => {
    expect(intentLabel("note")).toBe("Note");
    expect(intentLabel("question")).toBe("Question");
    expect(intentLabel("todo")).toBe("To-do");
    expect(intentLabel("flag-for-agent")).toBe("Flag for agent");
    expect(intentLabel("tour-stop")).toBe("Tour stop");
  });

  it("degrades to the raw value for an unrecognized intent", () => {
    expect(intentLabel("urgent")).toBe("urgent");
  });
});

describe("anchorBadgeLabel", () => {
  it("labels a line/symbol annotation as L{line}", () => {
    expect(anchorBadgeLabel({ anchor_kind: "line", line: 7, line_end: null })).toBe("L7");
    expect(anchorBadgeLabel({ anchor_kind: "symbol", line: 7, line_end: null })).toBe("L7");
  });

  it("labels a range annotation as L{line}–{line_end}", () => {
    expect(anchorBadgeLabel({ anchor_kind: "range", line: 10, line_end: 24 })).toBe("L10–24");
  });
});
