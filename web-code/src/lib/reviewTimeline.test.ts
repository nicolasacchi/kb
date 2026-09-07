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
      {
        at: 130,
        kind: "comment",
        annotation_id: "note-1",
        path: "a.rb",
        intent: "note",
        // V73-K2c: the wire's `author` is the v2 envelope object (the
        // server used to ALSO clobber it with a duplicate flat string —
        // see review_timeline.rs's fix — `authorNameOf` still tolerates a
        // bare string too, but this fixture reflects the corrected wire).
        author: { kind: "human", name: "you" },
        is_reply: false,
      },
      "myrepo",
      7,
    );
    expect(row.icon).toBe("comment");
    expect(row.label).toBe("comment · note · you");
    expect(row.href).toBe("/r/myrepo/~reviews/7/diff/a.rb?thread=note-1");
  });

  it("comment — a reply labels itself 'reply', not 'comment'", () => {
    const row = timelineRow(
      {
        at: 260,
        kind: "comment",
        annotation_id: "reply-1",
        path: "a.rb",
        intent: "question",
        author: { kind: "agent", name: "claude" },
        is_reply: true,
      },
      "r",
      1,
    );
    expect(row.label).toBe("reply · question · claude");
  });

  it("comment with no path still builds a diff href (no file segment)", () => {
    const row = timelineRow(
      {
        at: 130,
        kind: "comment",
        annotation_id: "note-1",
        path: "",
        intent: "note",
        author: { kind: "human", name: "you" },
        is_reply: false,
      },
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

// V73-K2c — review-timeline/2's five new lanes' kinds, plus the envelope
// (author/drift) every kind now carries.
describe("timelineRow — the V73-K2c kinds", () => {
  it("pr_body links into the pseudo-file view and carries a blob prefix", () => {
    const row = timelineRow(
      { at: 700, kind: "pr_body", path: "~review/pr-body.md", blob_sha: "0123456789abcdef" },
      "myrepo",
      7,
    );
    expect(row.icon).toBe("pr_body");
    expect(row.detail).toBe("0123456789");
    expect(row.href).toBe("/r/myrepo/~reviews/7/diff/~review/pr-body.md");
  });

  it("pr_body carries the drift caption when the wire sent one", () => {
    const row = timelineRow(
      {
        at: 700,
        kind: "pr_body",
        path: "~review/pr-body.md",
        blob_sha: "abc",
        drift: { kind: "pr_head", note: "the newest patchset moved past this snapshot" },
      },
      "r",
      1,
    );
    expect(row.driftNote).toBe("the newest patchset moved past this snapshot");
  });

  it("wt_comment is labelled distinctly from a review comment", () => {
    const row = timelineRow(
      {
        at: 800,
        kind: "wt_comment",
        path: "app/models/order.rb",
        intent: "note",
        author: { kind: "human", name: "you" },
        is_reply: false,
      },
      "myrepo",
      7,
    );
    expect(row.icon).toBe("wt_comment");
    expect(row.label).toBe("working-tree · comment · note · you");
    expect(row.detail).toBe("app/models/order.rb");
  });

  it("doc_revision names the revision and links to ~review/review.md", () => {
    const row = timelineRow(
      { at: 900, kind: "doc_revision", revision: 2, ps_number: 3, tier: "full" },
      "myrepo",
      7,
    );
    expect(row.icon).toBe("doc_revision");
    expect(row.label).toBe("review document revision 2 (ps3)");
    expect(row.detail).toBe("tier: full");
    expect(row.href).toBe("/r/myrepo/~reviews/7/diff/~review/review.md");
  });

  it("report names the verdict when present", () => {
    expect(timelineRow({ at: 950, kind: "report", verdict: "approve" }, "r", 1).label).toBe(
      "agent report: approve",
    );
    expect(timelineRow({ at: 950, kind: "report" }, "r", 1).label).toBe("agent report set");
  });

  it("claim names its kind, subject and ladder state", () => {
    const row = timelineRow(
      { at: 960, kind: "claim", claim_kind: "decision", subject: "checkout flow", state: "pinned" },
      "r",
      1,
    );
    expect(row.icon).toBe("claim");
    expect(row.label).toBe("decision on checkout flow");
    expect(row.detail).toBe("pinned");
  });

  it("github_comment links into the diff and carries the external html_url", () => {
    const row = timelineRow(
      { at: 970, kind: "github_comment", path: "a.rb", html_url: "https://github.com/x/y/pull/1#c1" },
      "myrepo",
      7,
    );
    expect(row.icon).toBe("github");
    expect(row.href).toBe("/r/myrepo/~reviews/7/diff/a.rb");
    expect(row.external).toBe("https://github.com/x/y/pull/1#c1");
  });

  it("turn names the tool + path + tier and links to the session's turn", () => {
    const row = timelineRow(
      {
        at: 980,
        kind: "turn",
        tool: "Edit",
        path: "app/models/order.rb",
        tier: "exact",
        session_id: "sess-1",
        turn_id: "t-0189aa112233",
      },
      "myrepo",
      7,
    );
    expect(row.icon).toBe("turn");
    expect(row.label).toBe("Edit touched app/models/order.rb");
    expect(row.detail).toBe("exact");
    expect(row.external).toContain("t-0189aa112233");
  });

  it("every kind carries the author register when the wire sends one", () => {
    const row = timelineRow(
      { at: 100, kind: "review_created", author: { kind: "system" } },
      "r",
      1,
    );
    expect(row.author).toEqual({ kind: "system" });
  });

  it("a malformed author object is never surfaced as the register", () => {
    const row = timelineRow(
      { at: 100, kind: "review_created", author: "not-an-object" as unknown as ReviewTimelineEvent["author"] },
      "r",
      1,
    );
    expect(row.author).toBeUndefined();
  });
});
