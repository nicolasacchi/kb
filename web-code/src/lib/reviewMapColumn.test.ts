import { describe, expect, it } from "vitest";
import type { ReviewFileRow, ReviewReadingStop } from "../api/types";
import { mapCensusText, mapChapters, mapRowTitle } from "./reviewMapColumn";

function file(path: string, over: Partial<ReviewFileRow> = {}): ReviewFileRow {
  return {
    path,
    old_path: null,
    status: "M",
    additions: 1,
    deletions: 0,
    blob_sha: "a".repeat(40),
    viewed: false,
    viewed_stale: false,
    open_annotations: 0,
    ...over,
  };
}

function stop(path: string, reason: string): ReviewReadingStop {
  return { path, reason, cycle: false };
}

describe("mapChapters", () => {
  it("groups CONSECUTIVE stops that share a reason", () => {
    const ordered = [file("a.rs"), file("b.rs"), file("c.rs"), file("d.rs")];
    const stops = [
      stop("a.rs", "no dependency signal"),
      stop("b.rs", "no dependency signal"),
      stop("c.rs", "test"),
      stop("d.rs", "no dependency signal"),
    ];
    expect(mapChapters(ordered, stops).map((c) => [c.reason, c.files.map((f) => f.path)])).toEqual([
      ["no dependency signal", ["a.rs", "b.rs"]],
      ["test", ["c.rs"]],
      // The FOURTH file shares the first chapter's reason but is NOT
      // folded back into it — re-bucketing would reorder the one thing the
      // reading order actually asserts.
      ["no dependency signal", ["d.rs"]],
    ]);
  });

  it("puts files the reading order never mentions in a named trailing group", () => {
    const chapters = mapChapters([file("a.rs"), file("z.rs")], [stop("a.rs", "test")]);
    expect(chapters.map((c) => [c.reason, c.files.map((f) => f.path)])).toEqual([
      ["test", ["a.rs"]],
      [null, ["z.rs"]],
    ]);
  });

  it("no reading order at all is ONE unnamed chapter, not zero", () => {
    const chapters = mapChapters([file("a.rs")], null);
    expect(chapters).toHaveLength(1);
    expect(chapters[0].reason).toBeNull();
    expect(mapChapters([file("a.rs")], [])).toEqual(chapters);
  });

  it("no files is no chapters", () => {
    expect(mapChapters([], null)).toEqual([]);
    expect(mapChapters([], [stop("a.rs", "test")])).toEqual([]);
  });

  it("preserves the caller's own file ORDER inside a chapter", () => {
    const ordered = [file("z.rs"), file("a.rs")];
    const stops = [stop("z.rs", "r"), stop("a.rs", "r")];
    expect(mapChapters(ordered, stops)[0].files.map((f) => f.path)).toEqual(["z.rs", "a.rs"]);
  });
});

describe("mapRowTitle", () => {
  it("states every fact the chips stand for", () => {
    expect(
      mapRowTitle({
        path: "src/lib.rs",
        viewed: true,
        viewedStale: false,
        openComments: 2,
        findings: 1,
        drafts: 3,
        noise: ["generated"],
      }),
    ).toBe("src/lib.rs · viewed · 2 open comments · 1 finding · 3 unpublished drafts · noise: generated");
  });

  it("says viewed-but-stale differently from viewed", () => {
    const base = {
      path: "a.rs",
      viewed: true,
      viewedStale: true,
      openComments: 0,
      findings: 0,
      drafts: 0,
      noise: [],
    };
    expect(mapRowTitle(base)).toContain("stale");
  });

  it("says unviewed rather than omitting the fact", () => {
    expect(
      mapRowTitle({
        path: "a.rs",
        viewed: false,
        viewedStale: false,
        openComments: 0,
        findings: 0,
        drafts: 0,
        noise: [],
      }),
    ).toBe("a.rs · unviewed");
  });
});

describe("mapCensusText", () => {
  it("pluralises honestly", () => {
    expect(mapCensusText(1, 0, 1)).toBe("1 file · 0 viewed · 1 chapter");
    expect(mapCensusText(9, 4, 3)).toBe("9 files · 4 viewed · 3 chapters");
  });
});
