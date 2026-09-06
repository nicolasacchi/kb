import { describe, expect, it } from "vitest";
import type { ReviewTimelineEvent } from "../api/types";
import { timelineRow, timelineRows } from "./reviewTimeline";

describe("timelineRow — every closed-vocab kind", () => {
  it("review_created", () => {
    const row = timelineRow({ at: 100, kind: "review_created", review_id: 1, repo: "r" }, "r", 1);
    expect(row).toEqual({ at: 100, kind: "review_created", icon: "created", label: "Review created" });
  });

  it("pr_bound", () => {
    const row = timelineRow(
      { at: 150, kind: "pr_bound", pr_number: 42, pr_repo_slug: "acme/widget" },
      "r",
      1,
    );
    expect(row.icon).toBe("pr");
    expect(row.label).toBe("PR #42 bound");
    expect(row.detail).toBe("acme/widget");
  });

  it("pr_bound degrades gracefully with no pr_number", () => {
    const row = timelineRow({ at: 150, kind: "pr_bound" }, "r", 1);
    expect(row.label).toBe("PR bound");
  });

  it("patchset", () => {
    const row = timelineRow(
      { at: 110, kind: "patchset", ps_number: 2, tip_sha: "8f21bb0bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb" },
      "r",
      1,
    );
    expect(row.icon).toBe("patchset");
    expect(row.label).toBe("ps2 captured");
    expect(row.detail).toBe("8f21bb0bbb");
  });

  it("findings_import", () => {
    const row = timelineRow(
      { at: 200, kind: "findings_import", import_batch_id: "batch_1", count: 2, slugs: ["f-a", "f-b"] },
      "r",
      1,
    );
    expect(row.icon).toBe("import");
    expect(row.label).toBe("2 findings imported");
    expect(row.detail).toBe("f-a, f-b");
  });

  it("findings_import pluralizes singular count", () => {
    const row = timelineRow({ at: 200, kind: "findings_import", count: 1, slugs: ["f-a"] }, "r", 1);
    expect(row.label).toBe("1 finding imported");
  });

  it("finding_added deep-links via reviewDiffHref finding=", () => {
    const row = timelineRow({ at: 220, kind: "finding_added", slug: "f-c", title: "some title" }, "myrepo", 7);
    expect(row.icon).toBe("finding");
    expect(row.label).toBe("finding added: f-c");
    expect(row.detail).toBe("some title");
    expect(row.href).toBe("/r/myrepo/~reviews/7/diff?finding=f-c");
  });

  it("disposition", () => {
    const row = timelineRow({ at: 250, kind: "disposition", slug: "f-b", state: "agree", by: "you" }, "myrepo", 7);
    expect(row.icon).toBe("disposition");
    expect(row.label).toBe("f-b → agree");
    expect(row.detail).toBe("by you");
    expect(row.href).toBe("/r/myrepo/~reviews/7/diff?finding=f-b");
  });

  it("verdict", () => {
    const row = timelineRow({ at: 500, kind: "verdict", state: "approve" }, "r", 1);
    expect(row).toEqual({ at: 500, kind: "verdict", icon: "verdict", label: "verdict set: approve" });
  });

  it("finding_published carries the external GitHub url plus an in-app deep link", () => {
    const row = timelineRow(
      { at: 400, kind: "finding_published", slug: "f-c", url: "https://github.com/x/y/pull/1#comment" },
      "myrepo",
      7,
    );
    expect(row.icon).toBe("published");
    expect(row.label).toBe("f-c published to GitHub");
    expect(row.href).toBe("/r/myrepo/~reviews/7/diff?finding=f-c");
    expect(row.external).toBe("https://github.com/x/y/pull/1#comment");
  });

  it("verdict_published has no in-app href, only external", () => {
    const row = timelineRow(
      { at: 450, kind: "verdict_published", url: "https://github.com/x/y/pull/1#review-1" },
      "r",
      1,
    );
    expect(row.icon).toBe("published");
    expect(row.label).toBe("verdict published to GitHub");
    expect(row.href).toBeUndefined();
    expect(row.external).toBe("https://github.com/x/y/pull/1#review-1");
  });

  it("comment — top-level note, labels kind · intent · author", () => {
    const row = timelineRow(
      { at: 130, kind: "comment", annotation_id: "note-1", path: "a.rb", intent: "note", author: "you", is_reply: false },
      "myrepo",
      7,
    );
    expect(row.icon).toBe("comment");
    expect(row.label).toBe("comment · note · you");
    expect(row.href).toBe("/r/myrepo/~reviews/7/diff/a.rb?thread=note-1");
  });

  it("comment — a reply labels itself 'reply', not 'comment'", () => {
    const row = timelineRow(
      { at: 260, kind: "comment", annotation_id: "reply-1", path: "a.rb", intent: "question", author: "claude", is_reply: true },
      "r",
      1,
    );
    expect(row.label).toBe("reply · question · claude");
  });

  it("comment with no path still builds a diff href (no file segment)", () => {
    const row = timelineRow(
      { at: 130, kind: "comment", annotation_id: "note-1", path: "", intent: "note", author: "you", is_reply: false },
      "myrepo",
      7,
    );
    expect(row.href).toBe("/r/myrepo/~reviews/7/diff?thread=note-1");
  });
});

describe("timelineRow — unknown kind never crashes", () => {
  it("degrades to a plain label for a future/unrecognized kind", () => {
    const row = timelineRow({ at: 999, kind: "some_future_kind", weird: { nested: true } }, "r", 1);
    expect(row).toEqual({ at: 999, kind: "some_future_kind", icon: "unknown", label: "some_future_kind" });
  });

  it("degrades even when kind itself is missing/non-string", () => {
    const row = timelineRow({ at: 999, kind: undefined as unknown as string }, "r", 1);
    expect(row.icon).toBe("unknown");
    expect(row.label).toBe("unknown event");
  });
});

describe("timelineRows", () => {
  it("maps a full event list in order, preserving indices", () => {
    const events: ReviewTimelineEvent[] = [
      { at: 100, kind: "review_created" },
      { at: 500, kind: "verdict", state: "comment" },
    ];
    const rows = timelineRows(events, "r", 1);
    expect(rows.map((r) => r.kind)).toEqual(["review_created", "verdict"]);
  });

  it("is empty for an empty event list", () => {
    expect(timelineRows([], "r", 1)).toEqual([]);
  });
});
