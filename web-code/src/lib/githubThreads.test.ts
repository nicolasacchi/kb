import { describe, expect, it } from "vitest";
import {
  githubOrphansForPath,
  githubThreadCountForPath,
  githubThreadsForPath,
  githubTimelineRows,
  indexGithubThreadsByLine,
  mergeTimelineRows,
  parseGithubTimestamp,
} from "./githubThreads";
import type { GithubThread } from "../api/types";
import type { TimelineRow } from "./reviewTimeline";

function baseComment(overrides: Partial<GithubThread> = {}): GithubThread {
  return {
    id: 1,
    author: "octocat",
    body: "looks fine",
    created_at: "2024-01-01T00:00:00Z",
    html_url: "https://github.com/x/y/pull/1#discussion_r1",
    path: "a.rb",
    side: "RIGHT",
    line: 5,
    original_line: 5,
    in_reply_to: null,
    resolved: { line: 5, confidence: "exact" },
    replies: [],
    ...overrides,
  };
}

describe("parseGithubTimestamp", () => {
  it("parses a valid ISO string to unix seconds", () => {
    expect(parseGithubTimestamp("2024-01-01T00:00:00Z")).toBe(1704067200);
  });
  it("is null for absent/unparseable input", () => {
    expect(parseGithubTimestamp(null)).toBeNull();
    expect(parseGithubTimestamp(undefined)).toBeNull();
    expect(parseGithubTimestamp("not a date")).toBeNull();
  });
});

describe("githubThreadsForPath / githubThreadCountForPath", () => {
  it("filters root threads by path", () => {
    const threads = [baseComment({ path: "a.rb" }), baseComment({ id: 2, path: "b.rb" })];
    expect(githubThreadsForPath(threads, "a.rb")).toHaveLength(1);
    expect(githubThreadCountForPath(threads, "b.rb")).toBe(1);
    expect(githubThreadCountForPath(threads, "c.rb")).toBe(0);
  });
});

describe("indexGithubThreadsByLine", () => {
  it("keys resolved threads by side + line, mapping LEFT to old and default to new", () => {
    const threads = [
      baseComment({ id: 1, side: "RIGHT", resolved: { line: 10, confidence: "exact" } }),
      baseComment({ id: 2, side: "LEFT", resolved: { line: 3, confidence: "fuzzy" } }),
      baseComment({ id: 3, side: undefined, resolved: { line: 7, confidence: "exact" } }),
    ];
    const idx = indexGithubThreadsByLine(threads, "a.rb");
    expect(idx.get("a.rb|new|10")?.[0].id).toBe(1);
    expect(idx.get("a.rb|old|3")?.[0].id).toBe(2);
    expect(idx.get("a.rb|new|7")?.[0].id).toBe(3);
  });

  it("excludes unresolved (general/orphaned) threads", () => {
    const threads = [baseComment({ resolved: undefined, general: true })];
    const idx = indexGithubThreadsByLine(threads, "a.rb");
    expect(idx.size).toBe(0);
  });
});

describe("githubOrphansForPath", () => {
  it("collects general and orphaned threads only", () => {
    const threads = [
      baseComment({ id: 1, general: true, resolved: undefined }),
      baseComment({ id: 2, orphaned: true, resolved: undefined }),
      baseComment({ id: 3, resolved: { line: 1, confidence: "exact" } }),
    ];
    const orphans = githubOrphansForPath(threads, "a.rb");
    expect(orphans.map((t) => t.id).sort()).toEqual([1, 2]);
  });
});

describe("githubTimelineRows", () => {
  it("emits one row per root comment and one per reply, tagged icon github", () => {
    const threads = [
      baseComment({
        id: 1,
        author: "alice",
        replies: [
          { id: 2, author: "bob", body: "reply", created_at: "2024-01-02T00:00:00Z", html_url: null, path: null, side: null, line: null, original_line: null, in_reply_to: 1 },
        ],
      }),
    ];
    const rows = githubTimelineRows(threads);
    expect(rows).toHaveLength(2);
    expect(rows[0].icon).toBe("github");
    expect(rows[0].label).toContain("alice commented on GitHub");
    expect(rows[1].label).toContain("bob replied on GitHub");
  });

  it("truncates a long body in the detail field", () => {
    const longBody = "x".repeat(200);
    const rows = githubTimelineRows([baseComment({ body: longBody })]);
    expect(rows[0].detail?.length).toBeLessThan(longBody.length);
    expect(rows[0].detail?.endsWith("…")).toBe(true);
  });
});

describe("mergeTimelineRows", () => {
  it("merges and sorts by at ascending", () => {
    const server: TimelineRow[] = [
      { at: 100, kind: "review_created", icon: "created", label: "created" },
      { at: 300, kind: "verdict", icon: "verdict", label: "verdict" },
    ];
    const gh: TimelineRow[] = [{ at: 200, kind: "github_comment", icon: "github", label: "gh" }];
    const merged = mergeTimelineRows(server, gh);
    expect(merged.map((r) => r.at)).toEqual([100, 200, 300]);
  });

  it("is stable for equal `at` values (server rows keep their relative order)", () => {
    const server: TimelineRow[] = [{ at: 100, kind: "a", icon: "comment", label: "a" }];
    const gh: TimelineRow[] = [{ at: 100, kind: "github_comment", icon: "github", label: "b" }];
    const merged = mergeTimelineRows(server, gh);
    expect(merged.map((r) => r.label)).toEqual(["a", "b"]);
  });
});
